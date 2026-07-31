use nix::fcntl::{open, OFlag};
use nix::sys::stat::Mode;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use libc::{c_void, mmap, munmap, pollfd, PROT_READ, MAP_SHARED, MAP_FAILED, POLLIN};
use anyhow::Result;
use std::time::Instant;

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
const VIDIOC_G_PARM: u64 = 0xc0cc5615;

const V4L2_CAP_TIMEPERFRAME: u32 = 0x1000;

const V4L2_BUF_TYPE_VIDEO_CAPTURE: u32 = 1;
const V4L2_MEMORY_MMAP: u32 = 1;

// FourCC pixel formats we know how to convert to IrFrame's u16-per-pixel representation.
// v4l2_fourcc('G','R','E','Y'), v4l2_fourcc('Y','U','Y','V'), v4l2_fourcc('Y','1','6',' ').
const V4L2_PIX_FMT_GREY: u32 = 0x59455247;
const V4L2_PIX_FMT_YUYV: u32 = 0x56595559;
const V4L2_PIX_FMT_Y16: u32 = 0x20363159;

const VIDIOC_QUERYCAP: u64 = 0x80685600;

// Kernel struct v4l2_capability: driver[16] + card[32] + bus_info[32] + version(4) +
// capabilities(4) + device_caps(4) + reserved[3](12) = 104 bytes.
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

/// Driver and card name via `VIDIOC_QUERYCAP` — doesn't request buffers or touch streaming
/// state, so it's safe to call against a device already in use elsewhere, unlike `Camera::open`.
pub fn query_caps(device_path: &str) -> Result<CameraCaps> {
    let fd = open(device_path, OFlag::O_RDWR, Mode::empty())
        .map_err(|e| anyhow::anyhow!("Failed to open {}: {}", device_path, e))?;
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let mut cap: v4l2_capability = unsafe { std::mem::zeroed() };
    ioctl(fd.as_raw_fd(), VIDIOC_QUERYCAP, &mut cap as *mut _ as *mut c_void)?;
    Ok(CameraCaps {
        driver: cstr_to_string(&cap.driver),
        card: cstr_to_string(&cap.card),
    })
}

/// Current width/height/pixelformat via `VIDIOC_G_FMT`, without the `REQBUFS`/mmap setup
/// `Camera::open` does — for diagnostics that just want to report the format, not capture.
pub fn query_format(device_path: &str) -> Result<(u32, u32, u32)> {
    let fd = open(device_path, OFlag::O_RDWR, Mode::empty())
        .map_err(|e| anyhow::anyhow!("Failed to open {}: {}", device_path, e))?;
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let mut fmt = make_v4l2_format(V4L2_BUF_TYPE_VIDEO_CAPTURE, 0, 0, 0);
    ioctl(fd.as_raw_fd(), VIDIOC_G_FMT, &mut fmt as *mut _ as *mut c_void)?;
    let pix: &v4l2_pix_format = unsafe { &*(fmt.raw.as_ptr() as *const v4l2_pix_format) };
    Ok((pix.width, pix.height, pix.pixelformat))
}

/// Human-readable FourCC (e.g. "GREY", "YUYV") for display in diagnostics.
pub fn fourcc_to_string(fourcc: u32) -> String {
    fourcc
        .to_le_bytes()
        .iter()
        .map(|&b| if b.is_ascii_graphic() || b == b' ' { b as char } else { '?' })
        .collect()
}

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

// Kernel struct v4l2_streamparm: type(4) + union{capture,output,raw_data[200]} = 204 bytes.
// Unlike v4l2_format, this union holds no pointer-typed members, so it needs no extra
// alignment padding after `type_`.
#[repr(C)]
struct v4l2_streamparm {
    type_: u32,
    raw: [u8; 200],
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

// A single capture with only 1 buffer works fine (one DQBUF, then stop), but a sustained
// stream (enroll's repeated capture_frame calls on one open Camera) needs the driver to have
// a free buffer to write the *next* frame into while the previous one is still checked out to
// userspace between DQBUF and QBUF. With only 1 buffer there's no such spare, and depending on
// driver timing DQBUF can hand back a buffer that hasn't actually been refilled yet — observed
// as frames with variance exactly 0. Multiple buffers give the driver room to keep filling
// buffers a capture loop hasn't gotten to yet.
const BUFFER_COUNT: u32 = 4;

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
        let t0 = Instant::now();
        let fd = open(device_path, OFlag::O_RDWR, Mode::empty())
            .map_err(|e| anyhow::anyhow!("Failed to open {}: {}", device_path, e))?;
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        eprintln!("TIMING_CAP open: {:?}", t0.elapsed());

        // Query current format instead of setting it.
        // VIDIOC_S_FMT triggers sensor init on IR cameras (~2s delay).
        // The camera is already configured correctly, so G_FMT is instant.
        let mut fmt = make_v4l2_format(V4L2_BUF_TYPE_VIDEO_CAPTURE, 0, 0, 0);
        ioctl(fd.as_raw_fd(), VIDIOC_G_FMT, &mut fmt as *mut _ as *mut c_void)?;
        let pix: &v4l2_pix_format = unsafe { &*(fmt.raw.as_ptr() as *const v4l2_pix_format) };
        let width = pix.width;
        let height = pix.height;
        let pixelformat = pix.pixelformat;
        match pixelformat {
            V4L2_PIX_FMT_GREY | V4L2_PIX_FMT_YUYV | V4L2_PIX_FMT_Y16 => {}
            other => {
                return Err(anyhow::anyhow!(
                    "Unsupported pixel format {:?} (0x{:08x}) on {} — expected GREY, YUYV, or Y16",
                    fourcc_to_string(other), other, device_path
                ));
            }
        }

        let mut reqbuf = v4l2_requestbuffers {
            count: BUFFER_COUNT,
            type_: V4L2_BUF_TYPE_VIDEO_CAPTURE,
            memory: V4L2_MEMORY_MMAP,
            reserved: [0, 0],
        };
        ioctl(fd.as_raw_fd(), VIDIOC_REQBUFS, &mut reqbuf as *mut _ as *mut c_void)?;

        // The driver may grant fewer buffers than requested; map exactly what it reports back.
        let mut buffers = Vec::with_capacity(reqbuf.count as usize);
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
                return Err(anyhow::anyhow!("mmap failed"));
            }
            buffers.push(MappedBuffer { ptr, length });
        }

        Ok(Self { fd, buffers, width, height, pixelformat, stream_on: false })
    }

    pub fn capture_frame(&mut self, timeout_ms: i32) -> Result<IrFrame> {
        if !self.stream_on {
            // Queue every mapped buffer up front so the driver always has somewhere to write
            // the next frame while previously-dequeued buffers are still being read.
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
        if ioctl(self.fd.as_raw_fd(), VIDIOC_DQBUF, &mut buf as *mut _ as *mut c_void).is_ok() {
            let mapped = &self.buffers[buf.index as usize];
            let bytes_used = buf.bytesused as usize;
            let data_slice = unsafe { std::slice::from_raw_parts(mapped.ptr as *const u8, bytes_used) };
            let data = convert_frame_bytes(self.pixelformat, data_slice);

            // Requeue this same buffer (VIDIOC_QBUF reuses the index/type/memory DQBUF just
            // filled in) so the driver can write the next frame into it.
            let _ = ioctl(self.fd.as_raw_fd(), VIDIOC_QBUF, &mut buf as *mut _ as *mut c_void);
            Ok(IrFrame { data, width: self.width, height: self.height })
        } else {
            Err(anyhow::anyhow!("Failed to capture frame: {}", std::io::Error::last_os_error()))
        }
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
        for buffer in &self.buffers {
            unsafe { munmap(buffer.ptr, buffer.length) };
        }
    }
}

pub fn capture_ir_frame(device_path: &str, timeout_ms: i32) -> Result<IrFrame> {
    let mut cam = Camera::open(device_path)?;
    let frame = cam.capture_frame(timeout_ms)?;
    cam.stop_stream();
    Ok(frame)
}

/// Converts raw captured bytes to IrFrame's u16-per-pixel form, based on the format the driver
/// reported at open time. GREY and the Y plane of YUYV are both native 8-bit, so they're
/// upscaled by 257 (0..=255 -> 0..=65535) to fill the same dynamic range Y16 delivers natively —
/// downstream code (CLAHE's `/257` bucketing, the *257 upscale in capture) assumes 16-bit-ranged
/// input regardless of which format produced it.
fn convert_frame_bytes(pixelformat: u32, data_slice: &[u8]) -> Vec<u16> {
    match pixelformat {
        V4L2_PIX_FMT_YUYV => data_slice
            .iter()
            .step_by(2) // Y0 U Y1 V ... — luma bytes sit at even offsets.
            .map(|&y| (y as u16) * 257)
            .collect(),
        V4L2_PIX_FMT_Y16 => data_slice
            .chunks_exact(2)
            .map(|c| u16::from_ne_bytes([c[0], c[1]]))
            .collect(),
        // V4L2_PIX_FMT_GREY, and the fallback for anything Camera::open already validated.
        _ => data_slice.iter().map(|&b| (b as u16) * 257).collect(),
    }
}

fn ioctl(fd: i32, request: u64, arg: *mut c_void) -> Result<i32> {
    let ret = unsafe { libc::ioctl(fd, request as libc::Ioctl, arg) };
    if ret < 0 {
        Err(anyhow::anyhow!("ioctl failed: {}", std::io::Error::last_os_error()))
    } else {
        Ok(ret)
    }
}

/// Queries the driver for the camera's native frame interval via `VIDIOC_G_PARM`, rather than
/// assuming any particular hardware's frame rate. Different IR sensors run at very different
/// native rates (this project's dev hardware is 15fps/~66ms; others may be 30fps/~33ms or
/// faster), so hardcoding a single "safe" sleep interval would either throttle faster cameras
/// unnecessarily or hammer slower ones with no benefit. Returns `None` if the driver doesn't
/// report `V4L2_CAP_TIMEPERFRAME` or the query otherwise fails — callers should fall back to a
/// conservative default in that case.
pub fn query_frame_interval_ms(device_path: &str) -> Option<u64> {
    let fd = open(device_path, OFlag::O_RDWR, Mode::empty()).ok()?;
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };

    let mut param = v4l2_streamparm {
        type_: V4L2_BUF_TYPE_VIDEO_CAPTURE,
        raw: [0; 200],
    };
    ioctl(fd.as_raw_fd(), VIDIOC_G_PARM, &mut param as *mut _ as *mut c_void).ok()?;

    let capability = u32::from_ne_bytes(param.raw[0..4].try_into().ok()?);
    if capability & V4L2_CAP_TIMEPERFRAME == 0 {
        return None;
    }

    let numerator = u32::from_ne_bytes(param.raw[8..12].try_into().ok()?);
    let denominator = u32::from_ne_bytes(param.raw[12..16].try_into().ok()?);
    if denominator == 0 {
        return None;
    }

    Some((numerator as u64 * 1000) / denominator as u64)
}

/// Resolves the physical bus path a V4L2 device is attached to, by canonicalizing its sysfs
/// `device` symlink (e.g. `/sys/class/video4linux/video2/device` resolves to something like
/// `/sys/devices/pci0000:00/0000:00:14.0/usb3/3-7/3-7:1.2`). This is the same identity
/// `udevadm`'s `ID_PATH` is derived from: the specific USB port chain (or PCI/MIPI path) the
/// device is physically wired to, which a spoofed device can't replicate just by claiming the
/// same VID/PID — it would have to physically intercept traffic on that exact bus segment.
/// Read directly from sysfs rather than shelling out to udevadm, since this can run from a
/// PAM-invoked process that isn't guaranteed a populated `PATH`.
pub fn device_bus_path(video_path: &str) -> anyhow::Result<String> {
    let kernel_name = video_kernel_name(video_path)?;
    let device_link = format!("/sys/class/video4linux/{}/device", kernel_name);
    let bus_path = std::fs::canonicalize(&device_link)
        .map_err(|e| anyhow::anyhow!("failed to resolve {}: {}", device_link, e))?;
    Ok(bus_path.to_string_lossy().into_owned())
}

/// Reads the V4L2 `index` sysfs attribute for a device — the driver-assigned function index
/// that disambiguates multiple nodes sharing one physical interface. Some cameras (including
/// the one this was written against) expose both a real capture node and a paired UVC metadata
/// node at the *same* bus path, so the bus path alone isn't enough to pin the right one.
pub fn device_capture_index(video_path: &str) -> anyhow::Result<u32> {
    let kernel_name = video_kernel_name(video_path)?;
    let index_path = format!("/sys/class/video4linux/{}/index", kernel_name);
    let raw = std::fs::read_to_string(&index_path)
        .map_err(|e| anyhow::anyhow!("failed to read {}: {}", index_path, e))?;
    raw.trim()
        .parse::<u32>()
        .map_err(|e| anyhow::anyhow!("bad index value in {}: {}", index_path, e))
}

fn video_kernel_name(video_path: &str) -> anyhow::Result<String> {
    let real = std::fs::canonicalize(video_path)
        .map_err(|e| anyhow::anyhow!("failed to resolve {}: {}", video_path, e))?;
    let name = real
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("{} has no file name", real.display()))?;
    Ok(name.to_string_lossy().into_owned())
}

/// Resolves a V4L2 device's USB vendor:product ID by walking up from its sysfs bus path
/// (`device_bus_path` resolves to the USB *interface* directory, e.g. `.../3-7:1.2`) until an
/// ancestor directory exposing `idVendor`/`idProduct` is found — that's the parent USB *device*
/// directory (`.../3-7`). Not all capture devices are USB (PCI, MIPI/CSI on other platforms), so
/// this can legitimately fail; callers should treat that as "no VID:PID to report", not an error
/// in the device itself.
pub fn usb_ids(video_path: &str) -> anyhow::Result<(String, String)> {
    let bus_path = device_bus_path(video_path)?;
    let mut dir = std::path::PathBuf::from(&bus_path);
    loop {
        let vendor = dir.join("idVendor");
        let product = dir.join("idProduct");
        if vendor.is_file() && product.is_file() {
            let vid = std::fs::read_to_string(&vendor)?.trim().to_string();
            let pid = std::fs::read_to_string(&product)?.trim().to_string();
            return Ok((vid, pid));
        }
        dir = match dir.parent() {
            Some(p) => p.to_path_buf(),
            None => anyhow::bail!("no idVendor/idProduct found walking up from {}", bus_path),
        };
    }
}

/// Lists every `/dev/videoN` node under `/sys/class/video4linux`, in ascending index order —
/// the enumeration diagnostics (`face-camera-diag`) build their per-device report from.
pub fn list_video_devices() -> Vec<String> {
    let base = std::path::Path::new("/sys/class/video4linux");
    let mut devices: Vec<String> = std::fs::read_dir(base)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| entry.file_name().to_str().map(|s| format!("/dev/{}", s)))
                .collect()
        })
        .unwrap_or_default();
    devices.sort_by_key(|path| {
        path.trim_start_matches("/dev/video").parse::<u32>().unwrap_or(u32::MAX)
    });
    devices
}

pub fn detect_ir_camera() -> Option<String> {
    let base = std::path::Path::new("/sys/class/video4linux");
    if let Ok(entries) = std::fs::read_dir(base) {
        for entry in entries.flatten() {
            let name_path = entry.path().join("name");
            if let Ok(name) = std::fs::read_to_string(&name_path) {
                if name.to_lowercase().contains("ir") || name.to_lowercase().contains("infrared") {
                    if let Some(device_name) = entry.file_name().to_str() {
                        return Some(format!("/dev/{}", device_name));
                    }
                }
            }
        }
    }
    None
}
