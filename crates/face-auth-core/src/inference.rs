use std::sync::Arc;
use tract_onnx::prelude::*;

#[cfg(feature = "npu")]
use openvino::{Core as OvCore, DeviceType, ElementType, InferRequest, PartialShape, Shape, Tensor as OvTensor};

#[cfg(feature = "npu")]
const ENCODER_SIZE: i64 = 112;

/// Where `deploy.sh` installs ovfetch's OpenVINO prefix. The auth binary
/// carries it as its rpath too, which is how OpenVINO's own dependencies
/// (TBB) are found in glibc's secure mode, where their `$ORIGIN` is ignored.
pub const OPENVINO_DIR: &str = "/usr/local/lib/face-auth/openvino";

/// Where to look for `libopenvino_c`, in order: ovfetch's prefix, an archive
/// install's copy (beside it, registered with `ldconfig`), then a distro
/// package. Fixed paths only, never the environment: PAM callers' environment
/// is scrubbed, and a set-group-ID binary must not load a library a caller
/// named.
#[cfg(feature = "npu")]
const OPENVINO_SEARCH: &[&str] = &[
    OPENVINO_DIR,
    "/usr/local/lib/face-auth/openvino/intel64",
    "/usr/lib64",
    "/usr/lib/x86_64-linux-gnu",
    "/lib/x86_64-linux-gnu",
    "/usr/lib",
];

/// OpenVINO isn't installed anywhere `OPENVINO_SEARCH` looks. The one
/// OpenVINO failure the backends fall back to tract for: the same model runs
/// on the CPU, so nothing about matching changes, only speed. A library that
/// is there but won't load is a broken install, and fails closed.
#[derive(Debug)]
pub struct OpenVinoMissing;

impl std::fmt::Display for OpenVinoMissing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "OpenVINO is not installed (no libopenvino_c in {OPENVINO_DIR} or the system library directories)")
    }
}

impl std::error::Error for OpenVinoMissing {}

/// The `libopenvino_c` to load from `dir`: the unversioned name if the
/// directory has it (ovfetch's prefix does), else the highest versioned one
/// (a distro package without its -devel symlink).
pub fn openvino_c_in(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let plain = dir.join("libopenvino_c.so");
    if plain.is_file() {
        return Some(plain);
    }
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let version: u64 = name.strip_prefix("libopenvino_c.so.")?.parse().ok()?;
            e.path().is_file().then_some((version, e.path()))
        })
        .max_by_key(|(version, _)| *version)
        .map(|(_, path)| path)
}

/// The `libopenvino_c` the backend would load, if OpenVINO is installed.
#[cfg(feature = "npu")]
pub fn find_openvino() -> Option<std::path::PathBuf> {
    OPENVINO_SEARCH.iter().find_map(|dir| openvino_c_in(std::path::Path::new(dir)))
}

/// Load the OpenVINO C API, once per process, before the first Core.
#[cfg(feature = "npu")]
fn load_openvino() -> anyhow::Result<()> {
    let Some(path) = find_openvino() else {
        return Err(OpenVinoMissing.into());
    };
    openvino_sys::library::load_from(&path)
        .map_err(|e| anyhow::anyhow!("loading OpenVINO from {}: {e}", path.display()))?;
    tracing::debug!(library = %path.display(), "OpenVINO loaded");
    Ok(())
}

/// Where `deploy.sh` copies the user's `ovfetch npu install` prefix: the NPU
/// compiler a distro's driver package lacks (Fedora's 1.32.0 rpm ships none,
/// and every `compile_model` fails with `ZE_RESULT_ERROR_UNSUPPORTED_FEATURE`),
/// or a whole user-mode driver. Root-owned and hash-checked like the OpenVINO
/// prefix on the rpath; a PAM caller cannot read the user's own prefix.
pub const NPU_PREFIX_DIR: &str = "/usr/local/lib/face-auth/npu";
/// What ovfetch writes there: the libraries to load, one per line, in order.
pub const NPU_PRELOAD_LIST: &str = "ovfetch-preload.txt";

/// The libraries to `dlopen` for the prefix at `dir`, in the list's order.
/// ovfetch writes an absolute path for each of the prefix's own files and a
/// bare soname for the host's Level Zero loader. `deploy.sh` copies the
/// prefix out of the user's home without rewriting the list, so an absolute
/// entry is re-rooted to `dir` by file name: nothing in the list can make the
/// auth process load from outside the root-owned prefix, and a soname goes
/// through the loader's normal search, as the driver's own `dlopen` would.
pub fn npu_preload_entries(list: &str, dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    list.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter_map(|l| {
            if l.starts_with('/') {
                std::path::Path::new(l).file_name().map(|name| dir.join(name))
            } else {
                Some(std::path::PathBuf::from(l))
            }
        })
        .collect()
}

/// Load the NPU prefix, if `deploy.sh` installed one, before OpenVINO first
/// opens the NPU. `RTLD_GLOBAL`, so that when the driver later asks for a
/// library by soname the dynamic linker returns the copy already loaded.
/// The order is the file's: a driver loaded before the Level Zero loader
/// crashed on its first call into it (ovfetch's README, measured 2026-10-24).
/// A library that will not load is an error, and the backend fails closed to
/// the password, which is also what the NPU would do without its compiler.
/// Runs after `load_openvino`, so OpenVINO's TBB is already in.
#[cfg(feature = "npu")]
fn preload_npu_prefix() -> anyhow::Result<()> {
    use std::io::ErrorKind;
    let dir = std::path::Path::new(NPU_PREFIX_DIR);
    let list = dir.join(NPU_PRELOAD_LIST);
    let text = match std::fs::read_to_string(&list) {
        Ok(text) => text,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(anyhow::Error::new(e).context(format!("reading {}", list.display()))),
    };
    for entry in npu_preload_entries(&text, dir) {
        use std::os::unix::ffi::OsStrExt;
        let name = std::ffi::CString::new(entry.as_os_str().as_bytes())?;
        // SAFETY: a valid NUL-terminated path; the handle is never closed, the
        // library stays loaded for the life of the process like OpenVINO.
        let handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) };
        if handle.is_null() {
            // SAFETY: dlerror returns NULL or a valid C string owned by libc.
            let reason = unsafe {
                let msg = libc::dlerror();
                if msg.is_null() {
                    "unknown error".to_owned()
                } else {
                    std::ffi::CStr::from_ptr(msg).to_string_lossy().into_owned()
                }
            };
            anyhow::bail!("loading {} (listed in {}): {reason}", entry.display(), list.display());
        }
        tracing::debug!(library = %entry.display(), "NPU prefix library loaded");
    }
    Ok(())
}

/// Run `f` on the process's one OpenVINO Core, shared by the encoder and the
/// detector. With a Core each, compiling the second model on the NPU can tear
/// down the Level Zero context the first one's request runs in (seen once the
/// NPU has been busy with other programs, until a reboot), and that model's
/// next inference segfaults inside libze_loader.
#[cfg(feature = "npu")]
pub(crate) fn with_ov_core<T>(f: impl FnOnce(&mut OvCore) -> anyhow::Result<T>) -> anyhow::Result<T> {
    static CORE: std::sync::Mutex<Option<OvCore>> = std::sync::Mutex::new(None);
    let mut core = CORE.lock().unwrap_or_else(|e| e.into_inner());
    if core.is_none() {
        load_openvino()?;
        preload_npu_prefix()?;
        *core = Some(OvCore::new()?);
    }
    f(core.as_mut().expect("set above"))
}

/// Load OpenVINO the way a scan does (library, NPU prefix, Core) and list
/// the devices it sees, with the library it loaded. For `vinoauthface-auth
/// --openvino-devices`: run as a user, it shows the libraries load in the
/// set-group-ID binary's secure mode.
#[cfg(feature = "npu")]
pub fn openvino_devices() -> anyhow::Result<(std::path::PathBuf, Vec<String>)> {
    let devices = with_ov_core(|core| Ok(core.available_devices()?.iter().map(|d| d.to_string()).collect()))?;
    let lib = find_openvino().ok_or(OpenVinoMissing)?;
    Ok((lib, devices))
}

// One encoder per process, so the size gap between variants costs nothing.
#[cfg_attr(feature = "npu", allow(clippy::large_enum_variant))]
pub enum FaceEncoder {
    Tract(Arc<TypedRunnableModel>),
    #[cfg(feature = "npu")]
    OpenVino {
        request: InferRequest,
        input_name: String,
        output_name: String,
    },
}

impl FaceEncoder {
    /// `backend` is "tract" (pure-Rust CPU) or "openvino", which needs a build
    /// with the `npu` feature and runs on `device` ("NPU", "GPU" or "CPU").
    pub fn new(model_path: &str, backend: &str, device: &str) -> anyhow::Result<Self> {
        // Check first: tract's own error for a missing file is opaque, and
        // this is the most common misconfiguration.
        if !std::path::Path::new(model_path).exists() {
            anyhow::bail!("face encoder model not found at {model_path}");
        }
        match backend {
            "openvino" => match Self::new_openvino(model_path, device) {
                Err(e) if e.is::<OpenVinoMissing>() => {
                    tracing::warn!("{e}; running the encoder on tract instead");
                    Self::new_tract(model_path)
                }
                other => other,
            },
            _ => Self::new_tract(model_path),
        }
    }

    fn new_tract(model_path: &str) -> anyhow::Result<Self> {
        // into_optimized() runs tract's fusion and lowering passes. Without it
        // the graph executes op-by-op as written: this model took ~400ms per
        // frame unoptimized, which dominated unlock latency.
        let model = onnx()
            .model_for_path(model_path)?
            .with_input_fact(
                0,
                InferenceFact::dt_shape(f32::datum_type(), tvec!(1, 3, 112, 112)),
            )?
            .into_optimized()?
            .into_runnable()?;
        Ok(Self::Tract(model))
    }

    #[cfg(feature = "npu")]
    fn new_openvino(model_path: &str, device: &str) -> anyhow::Result<Self> {
        let t0 = std::time::Instant::now();
        let (request, input_name, output_name) = with_ov_core(|core| {
            let mut model = core.read_model_from_file(model_path, "")?;
            let input_name = model.get_input_by_index(0)?.get_name()?;
            let output_name = model.get_output_by_index(0)?.get_name()?;
            model.reshape_single_input(&PartialShape::new_static(4, &[1, 3, ENCODER_SIZE, ENCODER_SIZE])?)?;
            let mut compiled = core.compile_model(&model, DeviceType::from(device))?;
            Ok((compiled.create_infer_request()?, input_name, output_name))
        })?;
        tracing::debug!(device, compile_ms = t0.elapsed().as_millis() as u64, "encoder compiled (openvino)");
        Ok(Self::OpenVino { request, input_name, output_name })
    }

    #[cfg(not(feature = "npu"))]
    fn new_openvino(_model_path: &str, _device: &str) -> anyhow::Result<Self> {
        anyhow::bail!("backend = \"openvino\" but this build was compiled without the `npu` feature")
    }

    /// Run the encoder and return an L2-normalised embedding.
    pub fn encode(&mut self, input: tract_ndarray::ArrayView3<f32>) -> anyhow::Result<Vec<f32>> {
        let embedding: Vec<f32> = match self {
            Self::Tract(model) => {
                let mut input = input.to_owned().into_dyn();
                input.insert_axis_inplace(tract_ndarray::Axis(0));
                let result = model.run(tvec!(Tensor::from(input).into_tvalue()))?;
                result[0].to_plain_array_view::<f32>()?.iter().copied().collect()
            }
            #[cfg(feature = "npu")]
            Self::OpenVino { request, input_name, output_name } => {
                let owned = input.as_standard_layout().to_owned();
                let data = owned
                    .as_slice()
                    .ok_or_else(|| anyhow::anyhow!("non-contiguous encoder input"))?;
                let shape = Shape::new(&[1, 3, ENCODER_SIZE, ENCODER_SIZE])?;
                let mut tensor = OvTensor::new(ElementType::F32, &shape)?;
                tensor.get_data_mut::<f32>()?.copy_from_slice(data);
                request.set_tensor(input_name, &tensor)?;
                request.infer()?;
                request.get_tensor(output_name)?.get_data::<f32>()?.to_vec()
            }
        };

        anyhow::ensure!(!embedding.is_empty(), "encoder returned an empty embedding");

        // A zero or non-finite norm would divide into NaN/inf and produce an
        // embedding that fails every comparison for reasons nothing reports.
        let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
        anyhow::ensure!(
            norm.is_finite() && norm > 0.0,
            "encoder produced a degenerate embedding (norm {norm})"
        );

        Ok(embedding.into_iter().map(|x| x / norm).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::{npu_preload_entries, openvino_c_in};
    use std::path::{Path, PathBuf};

    #[test]
    fn preload_entries_are_rerooted_to_the_installed_prefix_in_order() {
        // A full install's list, as ovfetch writes it into the user's prefix.
        let list = "libze_loader.so.1\n\
                    /home/k/.local/lib/ovfetch-npu/libopenvino_intel_npu_compiler.so\n\
                    /home/k/.local/lib/ovfetch-npu/libopenvino_intel_npu_compiler_loader.so\n\
                    \n\
                    /home/k/.local/lib/ovfetch-npu/libze_intel_npu.so.1\n";
        let dir = Path::new("/usr/local/lib/face-auth/npu");
        assert_eq!(
            npu_preload_entries(list, dir),
            vec![
                PathBuf::from("libze_loader.so.1"),
                dir.join("libopenvino_intel_npu_compiler.so"),
                dir.join("libopenvino_intel_npu_compiler_loader.so"),
                dir.join("libze_intel_npu.so.1"),
            ]
        );
    }

    #[test]
    fn a_list_cannot_point_outside_the_prefix() {
        let dir = Path::new("/usr/local/lib/face-auth/npu");
        let entries = npu_preload_entries("/tmp/evil/libnpu_driver_compiler.so\n/\n", dir);
        assert_eq!(entries, vec![dir.join("libnpu_driver_compiler.so")]);
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vaf-ov-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn openvino_c_prefers_the_plain_name_then_the_newest_version() {
        let dir = scratch("pick");
        assert_eq!(openvino_c_in(&dir), None);
        for name in ["libopenvino_c.so.2541", "libopenvino_c.so.2641", "libopenvino_c.so.9.bak", "libopenvino.so.2641"] {
            std::fs::write(dir.join(name), b"").unwrap();
        }
        assert_eq!(openvino_c_in(&dir), Some(dir.join("libopenvino_c.so.2641")));
        std::fs::write(dir.join("libopenvino_c.so"), b"").unwrap();
        assert_eq!(openvino_c_in(&dir), Some(dir.join("libopenvino_c.so")));
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(openvino_c_in(Path::new("/nonexistent/vaf")), None);
    }
}
