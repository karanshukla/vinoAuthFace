use nix::fcntl::{open, OFlag};
use nix::sys::stat::Mode;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use libc::{c_void, mmap, munmap, pollfd, PROT_READ, MAP_SHARED, MAP_FAILED, POLLIN};
use anyhow::Result;

// Correct ioctl numbers from kernel headers (x86_64)
// NOTE: This module is x86_64-only. ioctl numbers and struct layouts are ABI-dependent.
// For ARM/aarch64 support, replace with the `v4l` or `v4l2-sys` crates.
const VIDIOC_G_FMT: u64 = 0xc0d05604;
const VIDIOC_REQBUFS: u64 = 0xc0145608;
const VIDIOC_QUERYBUF: u64 = 0xc0585609;
const VIDIOC_QBUF: u64 = 0xc058560f;
const VIDIOC_DQBUF: u64 = 0xc0585611;
const VIDIOC_STREAMON: u64 = 0x40045612;
const VIDIOC_STREAMOFF: u64 = 0x40045613;
const VIDIOC_QUERYCAP: u64 = 0x80685600;

const V4L2_BUF_TYPE_VIDEO_CAPTURE: u32 = 1;
const V4L2_MEMORY_MMAP: u32 = 1;

/// v4l2_fourcc('G','R','E','Y') — 8-bit greyscale, what most Windows Hello
/// IR sensors expose.
const V4L2_PIX_FMT_GREY: u32 = 0x5945_5247;
/// v4l2_fourcc('Y','U','Y','V') — some IR sensors report packed YUYV with the
/// IR intensity in the luma bytes.
const V4L2_PIX_FMT_YUYV: u32 = 0x5659_5559;
/// v4l2_fourcc('Y','1','6',' ') — 16-bit little-endian greyscale.
const V4L2_PIX_FMT_Y16: u32 = 0x2036_3159;

/// Formats `convert_frame_bytes` knows how to turn into luma samples. Anything
/// else (MJPEG, RGB) is refused rather than reinterpreted as noise.
fn supported_format(fourcc: u32) -> bool {
    matches!(fourcc, V4L2_PIX_FMT_GREY | V4L2_PIX_FMT_YUYV | V4L2_PIX_FMT_Y16)
}

/// Human-readable FourCC, e.g. "GREY".
pub fn fourcc_to_string(fourcc: u32) -> String {
    fourcc
        .to_le_bytes()
        .iter()
        .map(|&b| if b.is_ascii_graphic() || b == b' ' { b as char } else { '?' })
        .collect()
}

/// Convert raw buffer bytes to u16 luma samples. 8-bit sources are widened by
/// 257 (0..=255 -> 0..=65535) so every format lands in the same range the rest
/// of the pipeline expects.
fn convert_frame_bytes(pixelformat: u32, bytes: &[u8]) -> Vec<u16> {
    match pixelformat {
        // Y0 U Y1 V ...: luma sits at even offsets.
        V4L2_PIX_FMT_YUYV => bytes.iter().step_by(2).map(|&y| y as u16 * 257).collect(),
        V4L2_PIX_FMT_Y16 => bytes.as_chunks::<2>().0.iter().map(|&c| u16::from_le_bytes(c)).collect(),
        _ => bytes.iter().map(|&b| b as u16 * 257).collect(),
    }
}

/// Refuse implausible geometry from `G_FMT` before it sizes an allocation.
const MAX_DIMENSION: u32 = 8192;

/// Buffers to request from the driver.
///
/// One is not enough. Between `DQBUF` and the following `QBUF` the driver has
/// nowhere to put an incoming frame, so it drops it — which means two
/// successive captures are not necessarily adjacent frames. On a sensor that
/// strobes its illuminator on alternate frames, that makes the phase of what
/// you get unpredictable, and "take the brighter of two" can hand back two
/// unlit frames in a row. A small ring keeps a buffer queued at all times.
const BUFFER_COUNT: u32 = 4;

// Kernel struct v4l2_format: type(4) + padding(4) + union raw_data[200] = 208 bytes
#[repr(C)]
struct v4l2_format {
    type_: u32,
    _pad: [u8; 4],
    raw: [u8; 200],
}

fn make_v4l2_format(type_: u32, width: u32, height: u32, pixelformat: u32) -> v4l2_format {
    let mut fmt = v4l2_format {
        type_,
        _pad: [0; 4],
        raw: [0; 200],
    };
    let pix: &mut v4l2_pix_format = unsafe { &mut *(fmt.raw.as_mut_ptr() as *mut v4l2_pix_format) };
    pix.width = width;
    pix.height = height;
    pix.pixelformat = pixelformat;
    fmt
}

#[repr(C)]
struct v4l2_pix_format {
    width: u32,
    height: u32,
    pixelformat: u32,
    field: u32,
    bytesperline: u32,
    sizeimage: u32,
    colorspace: u32,
    priv_: u32,
    flags: u32,
    ycbcr_enc: u32,
    quantization: u32,
    xfer_func: u32,
}

#[repr(C)]
struct v4l2_requestbuffers {
    count: u32,
    type_: u32,
    memory: u32,
    reserved: [u32; 2],
}

// Kernel struct v4l2_buffer: 88 bytes
#[repr(C)]
struct v4l2_buffer {
    index: u32,
    type_: u32,
    bytesused: u32,
    flags: u32,
    field: u32,
    timestamp: [i64; 2],      // struct timeval
    timecode: [u32; 4],       // struct v4l2_timecode: type, flags, frames, seconds, minutes, hours, userbits[4] → packed as 4 u32
    sequence: u32,
    memory: u32,
    m: u64,                   // union { offset, userptr, planes, fd }
    length: u32,
    reserved2: u32,
    reserved: u32,
}

#[derive(Clone)]
pub struct IrFrame {
    pub data: Vec<u16>,
    pub width: u32,
    pub height: u32,
}

impl IrFrame {
    /// Mean sample value, 0.0–65535.0. Accumulated in f64 because a 640x400
    /// frame sums a quarter of a million terms.
    pub fn mean_intensity(&self) -> f64 {
        if self.data.is_empty() {
            return 0.0;
        }
        self.data.iter().map(|&v| v as f64).sum::<f64>() / self.data.len() as f64
    }
}

/// One mmap'd capture buffer.
struct MappedBuffer {
    ptr: *mut c_void,
    length: usize,
}

pub struct Camera {
    fd: OwnedFd,
    buffers: Vec<MappedBuffer>,
    width: u32,
    height: u32,
    pixelformat: u32,
    stream_on: bool,
}

unsafe impl Send for Camera {}

impl Camera {
    pub fn open(device_path: &str) -> Result<Self> {
        let fd = open(device_path, OFlag::O_RDWR, Mode::empty())
            .map_err(|e| anyhow::anyhow!("Failed to open {}: {}", device_path, e))?;
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };

        // Query current format instead of setting it.
        // VIDIOC_S_FMT triggers sensor init on IR cameras (~2s delay).
        // The camera is already configured correctly, so G_FMT is instant.
        let mut fmt = make_v4l2_format(V4L2_BUF_TYPE_VIDEO_CAPTURE, 0, 0, 0);
        ioctl(fd.as_raw_fd(), VIDIOC_G_FMT, &mut fmt as *mut _ as *mut c_void)?;
        let pix: &v4l2_pix_format = unsafe { &*(fmt.raw.as_ptr() as *const v4l2_pix_format) };
        let width = pix.width;
        let height = pix.height;
        let pixelformat = pix.pixelformat;

        // Validate what the driver reports rather than assuming it. Pointed at
        // an ordinary webcam streaming MJPEG this would otherwise reinterpret
        // compressed bytes as greyscale and compare the noise against a real
        // template.
        if !supported_format(pixelformat) {
            return Err(anyhow::anyhow!(
                "{} reports pixel format '{}' ({:#010x}); expected GREY, YUYV or Y16 from an \
                 IR sensor",
                device_path,
                fourcc_to_string(pixelformat),
                pixelformat
            ));
        }
        if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
            return Err(anyhow::anyhow!(
                "{} reports implausible frame geometry {}x{}",
                device_path,
                width,
                height
            ));
        }

        let mut reqbuf = v4l2_requestbuffers {
            count: BUFFER_COUNT,
            type_: V4L2_BUF_TYPE_VIDEO_CAPTURE,
            memory: V4L2_MEMORY_MMAP,
            reserved: [0, 0],
        };
        ioctl(fd.as_raw_fd(), VIDIOC_REQBUFS, &mut reqbuf as *mut _ as *mut c_void)?;
        if reqbuf.count == 0 {
            return Err(anyhow::anyhow!("{} allocated no capture buffers", device_path));
        }

        // The driver may grant fewer buffers than requested; honour what it says.
        let mut buffers: Vec<MappedBuffer> = Vec::with_capacity(reqbuf.count as usize);
        for index in 0..reqbuf.count {
            let mut buf = v4l2_buffer {
                index,
                type_: V4L2_BUF_TYPE_VIDEO_CAPTURE,
                memory: V4L2_MEMORY_MMAP,
                ..unsafe { std::mem::zeroed() }
            };
            ioctl(fd.as_raw_fd(), VIDIOC_QUERYBUF, &mut buf as *mut _ as *mut c_void)?;

            let length = buf.length as usize;
            let offset = buf.m as libc::off_t;
            let ptr = unsafe {
                mmap(
                    std::ptr::null_mut(),
                    length,
                    PROT_READ,
                    MAP_SHARED,
                    fd.as_raw_fd(),
                    offset,
                )
            };
            if ptr == MAP_FAILED {
                // Unmap whatever succeeded before giving up.
                for b in &buffers {
                    unsafe { munmap(b.ptr, b.length) };
                }
                return Err(anyhow::anyhow!("mmap failed for buffer {index}"));
            }
            buffers.push(MappedBuffer { ptr, length });
        }

        Ok(Self { fd, buffers, width, height, pixelformat, stream_on: false })
    }

    pub fn capture_frame(&mut self, timeout_ms: i32) -> Result<IrFrame> {
        if !self.stream_on {
            // Queue every buffer before streaming, so the driver always has
            // somewhere to write and never has to drop a frame.
            for index in 0..self.buffers.len() as u32 {
                let buf = v4l2_buffer {
                    index,
                    type_: V4L2_BUF_TYPE_VIDEO_CAPTURE,
                    memory: V4L2_MEMORY_MMAP,
                    ..unsafe { std::mem::zeroed() }
                };
                ioctl(self.fd.as_raw_fd(), VIDIOC_QBUF, &buf as *const _ as *mut c_void)?;
            }
            let stream_type = V4L2_BUF_TYPE_VIDEO_CAPTURE;
            ioctl(self.fd.as_raw_fd(), VIDIOC_STREAMON, &stream_type as *const _ as *mut c_void)?;
            self.stream_on = true;
        }

        // Use poll() to wait for data with the configured timeout
        let mut pfd = pollfd {
            fd: self.fd.as_raw_fd(),
            events: POLLIN,
            revents: 0,
        };
        let poll_ret = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
        if poll_ret < 0 {
            return Err(anyhow::anyhow!("poll failed: {}", std::io::Error::last_os_error()));
        }
        if poll_ret == 0 {
            return Err(anyhow::anyhow!("Capture timed out after {}ms", timeout_ms));
        }

        let mut buf = v4l2_buffer {
            type_: V4L2_BUF_TYPE_VIDEO_CAPTURE,
            memory: V4L2_MEMORY_MMAP,
            ..unsafe { std::mem::zeroed() }
        };
        if ioctl(self.fd.as_raw_fd(), VIDIOC_DQBUF, &mut buf as *mut _ as *mut c_void).is_err() {
            let err = std::io::Error::last_os_error();
            return Err(anyhow::anyhow!("Failed to capture frame: {}", err));
        }

        let index = buf.index as usize;
        let Some(mapped) = self.buffers.get(index) else {
            return Err(anyhow::anyhow!(
                "driver dequeued buffer index {index}, only {} are mapped",
                self.buffers.len()
            ));
        };

        // `bytesused` comes from the driver and is not trusted to fit the
        // mapping: this is the one place external data sizes an unsafe read,
        // and an oversized value would run off the end of the mmap.
        let bytes_used = (buf.bytesused as usize).min(mapped.length);
        if bytes_used < buf.bytesused as usize {
            tracing::warn!(
                reported = buf.bytesused,
                mapped = mapped.length,
                "driver reported more bytes than the buffer holds; truncating"
            );
        }
        let data_slice = unsafe { std::slice::from_raw_parts(mapped.ptr as *const u8, bytes_used) };
        let data = convert_frame_bytes(self.pixelformat, data_slice);

        // Hand the buffer straight back so the ring stays full.
        let _ = ioctl(self.fd.as_raw_fd(), VIDIOC_QBUF, &mut buf as *mut _ as *mut c_void);

        let expected = self.width as usize * self.height as usize;
        if data.len() < expected {
            return Err(anyhow::anyhow!(
                "short frame: got {} samples, expected {} for {}x{} {}",
                data.len(),
                expected,
                self.width,
                self.height,
                fourcc_to_string(self.pixelformat)
            ));
        }

        Ok(IrFrame { data, width: self.width, height: self.height })
    }

    /// Capture a frame, preferring an illuminated one.
    ///
    /// Windows Hello IR modules commonly strobe their illuminator, emitting a
    /// lit frame and a near-black ambient frame alternately. On the reference
    /// ASUS sensor the lit frames average 48–96 (of 255) and the dark ones
    /// 2–8, strictly alternating at 15 fps.
    ///
    /// A dark frame is not merely useless: histogram equalisation stretches its
    /// 0–23 range across the full scale and turns sensor noise into a
    /// high-contrast grey field, which the detector then searches in vain.
    /// Worse, a capture interval that happens to be an even number of frames
    /// locks onto one phase, so an unlucky caller sees *only* dark frames.
    ///
    /// Taking the brighter of two consecutive frames sidesteps all of that
    /// without assuming the strobe exists: on a camera that does not strobe the
    /// two frames are alike and either will do.
    pub fn capture_illuminated_frame(&mut self, timeout_ms: i32) -> Result<IrFrame> {
        let first = self.capture_frame(timeout_ms)?;

        // If the second capture fails, the first is still a usable answer.
        let second = match self.capture_frame(timeout_ms) {
            Ok(f) => f,
            Err(e) => {
                tracing::debug!("second frame of pair failed, using the first: {e}");
                return Ok(first);
            }
        };

        let (a, b) = (first.mean_intensity(), second.mean_intensity());
        tracing::trace!(first_mean = a, second_mean = b, "illumination pair");
        Ok(if b > a { second } else { first })
    }

    fn stop_stream(&mut self) {
        if self.stream_on {
            let stream_type = V4L2_BUF_TYPE_VIDEO_CAPTURE;
            let _ = ioctl(self.fd.as_raw_fd(), VIDIOC_STREAMOFF, &stream_type as *const _ as *mut c_void);
            self.stream_on = false;
        }
    }
}

impl Drop for Camera {
    fn drop(&mut self) {
        self.stop_stream();
        for b in &self.buffers {
            unsafe { munmap(b.ptr, b.length) };
        }
    }
}

pub fn capture_ir_frame(device_path: &str, timeout_ms: i32) -> Result<IrFrame> {
    let mut cam = Camera::open(device_path)?;
    let frame = cam.capture_illuminated_frame(timeout_ms)?;
    cam.stop_stream();
    Ok(frame)
}

fn ioctl(fd: i32, request: u64, arg: *mut c_void) -> Result<i32> {
    let ret = unsafe { libc::ioctl(fd, request as libc::Ioctl, arg) };
    if ret < 0 {
        Err(anyhow::anyhow!("ioctl failed: {}", std::io::Error::last_os_error()))
    } else {
        Ok(ret)
    }
}

// Kernel struct v4l2_capability: driver[16] + card[32] + bus_info[32] +
// version + capabilities + device_caps + reserved[3] = 104 bytes.
#[repr(C)]
struct v4l2_capability {
    driver: [u8; 16],
    card: [u8; 32],
    bus_info: [u8; 32],
    version: u32,
    capabilities: u32,
    device_caps: u32,
    reserved: [u32; 3],
}

pub struct CameraCaps {
    pub driver: String,
    pub card: String,
}

fn cstr_to_string(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// Driver and card name via `VIDIOC_QUERYCAP`. Touches no buffers or stream
/// state, so it is safe against a device already in use elsewhere.
pub fn query_caps(device_path: &str) -> Result<CameraCaps> {
    let fd = open(device_path, OFlag::O_RDWR, Mode::empty())
        .map_err(|e| anyhow::anyhow!("Failed to open {}: {}", device_path, e))?;
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let mut cap: v4l2_capability = unsafe { std::mem::zeroed() };
    ioctl(fd.as_raw_fd(), VIDIOC_QUERYCAP, &mut cap as *mut _ as *mut c_void)?;
    Ok(CameraCaps { driver: cstr_to_string(&cap.driver), card: cstr_to_string(&cap.card) })
}

/// Current width, height and FourCC via `VIDIOC_G_FMT`, without the buffer
/// setup `Camera::open` does.
pub fn query_format(device_path: &str) -> Result<(u32, u32, u32)> {
    let fd = open(device_path, OFlag::O_RDWR, Mode::empty())
        .map_err(|e| anyhow::anyhow!("Failed to open {}: {}", device_path, e))?;
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let mut fmt = make_v4l2_format(V4L2_BUF_TYPE_VIDEO_CAPTURE, 0, 0, 0);
    ioctl(fd.as_raw_fd(), VIDIOC_G_FMT, &mut fmt as *mut _ as *mut c_void)?;
    let pix: &v4l2_pix_format = unsafe { &*(fmt.raw.as_ptr() as *const v4l2_pix_format) };
    Ok((pix.width, pix.height, pix.pixelformat))
}

/// Every `/dev/videoN` under `/sys/class/video4linux`, in numeric order.
pub fn list_video_devices() -> Vec<String> {
    let mut devices: Vec<String> = std::fs::read_dir("/sys/class/video4linux")
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| e.file_name().to_str().map(|s| format!("/dev/{s}")))
                .collect()
        })
        .unwrap_or_default();
    devices.sort_by_key(|p| p.trim_start_matches("/dev/video").parse::<u32>().unwrap_or(u32::MAX));
    devices
}

/// USB vendor:product ID, found by walking up from the device's bus path (a
/// USB *interface*) to the parent USB *device* that carries `idVendor`. Not
/// every capture device is USB, so failure here is normal, not an error in
/// the device.
pub fn usb_ids(video_path: &str) -> Result<(String, String)> {
    let bus_path = device_bus_path(video_path)?;
    let mut dir = std::path::PathBuf::from(&bus_path);
    loop {
        let (vendor, product) = (dir.join("idVendor"), dir.join("idProduct"));
        if vendor.is_file() && product.is_file() {
            return Ok((
                std::fs::read_to_string(vendor)?.trim().to_string(),
                std::fs::read_to_string(product)?.trim().to_string(),
            ));
        }
        dir = match dir.parent() {
            Some(p) => p.to_path_buf(),
            None => anyhow::bail!("no idVendor/idProduct above {bus_path}"),
        };
    }
}

/// Physical bus path a V4L2 device is attached to, from its canonicalised
/// sysfs `device` link (e.g. `/sys/devices/pci0000:00/0000:00:14.0/usb3/3-7/3-7:1.2`).
///
/// This is the identity udev's `ID_PATH` derives from: the port chain the
/// device is wired to. A spoofed device can claim the real camera's VID/PID
/// but cannot sit on the same port at the same time. Read from sysfs directly
/// because a PAM-invoked process has no guaranteed `PATH` for `udevadm`.
pub fn device_bus_path(video_path: &str) -> Result<String> {
    let link = format!("/sys/class/video4linux/{}/device", video_kernel_name(video_path)?);
    let bus_path = std::fs::canonicalize(&link)
        .map_err(|e| anyhow::anyhow!("failed to resolve {link}: {e}"))?;
    Ok(bus_path.to_string_lossy().into_owned())
}

/// The V4L2 `index` attribute. UVC cameras often expose a capture node and a
/// metadata node at the *same* bus path, so the path alone is not enough.
pub fn device_capture_index(video_path: &str) -> Result<u32> {
    let path = format!("/sys/class/video4linux/{}/index", video_kernel_name(video_path)?);
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("failed to read {path}: {e}"))?;
    raw.trim()
        .parse()
        .map_err(|e| anyhow::anyhow!("bad index value in {path}: {e}"))
}

/// Kernel name (`videoN`) behind a device path, following symlinks such as the
/// `/dev/face-auth-ir` link `pin-camera.sh` creates.
fn video_kernel_name(video_path: &str) -> Result<String> {
    let real = std::fs::canonicalize(video_path)
        .map_err(|e| anyhow::anyhow!("failed to resolve {video_path}: {e}"))?;
    let name = real
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("{} has no file name", real.display()))?;
    Ok(name.to_string_lossy().into_owned())
}

/// Does this V4L2 device name look like an IR sensor?
///
/// Matched on word boundaries rather than as a substring: plain `contains("ir")`
/// also fires on "Virtual Camera" (v4l2loopback) and "BRIO", either of which
/// would be picked as the authentication camera ahead of the real IR sensor.
pub fn name_suggests_ir(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|word| word == "ir" || word == "infrared")
}

/// Is this node a physical sensor streaming native greyscale?
///
/// Catches IR sensors whose sysfs name says nothing useful (e.g. a combined
/// module named "Integrated_Webcam_FHD: Integrat", truncated at 32 bytes).
/// RGB webcams do not stream GREY or Y16. Virtual nodes are excluded: an
/// existing v4l2loopback device takes whatever format its writer sets, which
/// any local user can do.
fn is_physical_greyscale(path: &str) -> bool {
    let physical = device_bus_path(path).is_ok_and(|p| !p.contains("/virtual/"));
    physical
        && query_format(path)
            .is_ok_and(|(_, _, fourcc)| matches!(fourcc, V4L2_PIX_FMT_GREY | V4L2_PIX_FMT_Y16))
}

fn looks_like_ir(path: &str, name: &str) -> bool {
    name_suggests_ir(name) || is_physical_greyscale(path)
}

/// Enumerate IR capture devices, best candidate first.
///
/// UVC cameras commonly expose several `/dev/videoN` nodes under one name —
/// typically a capture node followed by a metadata node that accepts no
/// frames. Each candidate is opened to confirm it really is a capture device
/// in a supported format before being offered.
pub fn enumerate_ir_cameras() -> Vec<(String, String)> {
    let base = std::path::Path::new("/sys/class/video4linux");
    let mut candidates: Vec<(String, String)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(base) {
        for entry in entries.flatten() {
            let Ok(name) = std::fs::read_to_string(entry.path().join("name")) else {
                continue;
            };
            let name = name.trim().to_string();
            let Some(device_name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let path = format!("/dev/{}", device_name);
            if !looks_like_ir(&path, &name) {
                continue;
            }
            candidates.push((path, name));
        }
    }
    // An explicit IR name ranks ahead of a format-only match; within each,
    // sort numerically so /dev/video9 orders before /dev/video10.
    candidates.sort_by_key(|(path, name)| {
        (
            !name_suggests_ir(name),
            path.trim_start_matches("/dev/video")
                .parse::<u32>()
                .unwrap_or(u32::MAX),
            path.clone(),
        )
    });
    candidates
        .into_iter()
        .filter(|(path, _)| Camera::open(path).is_ok())
        .collect()
}

pub fn detect_ir_camera() -> Option<String> {
    enumerate_ir_cameras()
        .into_iter()
        .next()
        .map(|(path, _)| path)
}

/// Is `path` a real IR capture device on this machine?
///
/// Lets an unprivileged setting name *which* IR sensor to use without letting
/// it name an arbitrary video source: the device must live under `/dev`, carry
/// an IR-looking name in sysfs or be a physical greyscale sensor, and open as a
/// supported capture node. Selecting
/// among the sensors physically present is a preference; pointing the
/// authentication camera at some other stream is not.
pub fn is_ir_capture_device(path: &str) -> bool {
    let Some(node) = path.strip_prefix("/dev/") else {
        return false;
    };
    if node.is_empty() || node.contains('/') || node.contains("..") {
        return false;
    }

    let name_path = std::path::Path::new("/sys/class/video4linux")
        .join(node)
        .join("name");
    let Ok(name) = std::fs::read_to_string(name_path) else {
        return false;
    };
    if !looks_like_ir(path, name.trim()) {
        return false;
    }

    // Camera::open enforces a supported pixel format and sane geometry.
    Camera::open(path).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_ir_camera_names() {
        assert!(name_suggests_ir("Integrated IR Camera"));
        assert!(name_suggests_ir("Chicony USB2.0 Camera: Infrared"));
        assert!(name_suggests_ir("ir-camera"));
        assert!(name_suggests_ir("IR"));
    }

    #[test]
    fn converts_each_supported_format_to_widened_luma() {
        assert_eq!(convert_frame_bytes(V4L2_PIX_FMT_GREY, &[0, 1, 255]), vec![0, 257, 65535]);
        // Y0 U Y1 V: chroma bytes are dropped.
        assert_eq!(convert_frame_bytes(V4L2_PIX_FMT_YUYV, &[10, 99, 20, 99]), vec![2570, 5140]);
        assert_eq!(convert_frame_bytes(V4L2_PIX_FMT_Y16, &[0x34, 0x12]), vec![0x1234]);
    }

    #[test]
    fn refuses_compressed_formats() {
        let mjpg = u32::from_le_bytes(*b"MJPG");
        assert!(!supported_format(mjpg));
        assert_eq!(fourcc_to_string(mjpg), "MJPG");
        assert!(supported_format(V4L2_PIX_FMT_GREY));
    }

    #[test]
    fn does_not_match_ir_inside_unrelated_words() {
        // These are the cases plain substring matching got wrong.
        assert!(!name_suggests_ir("Virtual Camera"));
        assert!(!name_suggests_ir("Logitech BRIO"));
        assert!(!name_suggests_ir("Integrated Webcam"));
        assert!(!name_suggests_ir("Mirror Cam"));
    }
}
