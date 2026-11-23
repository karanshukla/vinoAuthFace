// Writes `paths.rs`: where the installed programs and data live.
//
// deploy.sh installs under /usr/local, so that is the default. A distro
// package (deb, rpm) builds with `VINOAUTHFACE_PREFIX=/usr` instead, since
// packages must not touch /usr/local. Everything the binaries run or read by
// absolute path (the set-group-ID PAM binary, the polkit-pinned helper, the
// models, OpenVINO's prefix) moves with it, so the paths polkit and PAM pin
// are the ones the package installed.
//
//   VINOAUTHFACE_PREFIX      default /usr/local
//   VINOAUTHFACE_LIBEXECDIR  default $PREFIX/libexec

use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=VINOAUTHFACE_PREFIX");
    println!("cargo:rerun-if-env-changed=VINOAUTHFACE_LIBEXECDIR");

    let prefix = env::var("VINOAUTHFACE_PREFIX").unwrap_or_else(|_| "/usr/local".into());
    let libexec = env::var("VINOAUTHFACE_LIBEXECDIR").unwrap_or_else(|_| format!("{prefix}/libexec"));
    // These end up in a set-group-ID binary and a polkit policy: refuse
    // anything that is not a plain absolute path.
    for (name, value) in [("VINOAUTHFACE_PREFIX", &prefix), ("VINOAUTHFACE_LIBEXECDIR", &libexec)] {
        let ok = value.starts_with('/')
            && value.len() > 1
            && !value.ends_with('/')
            && value.chars().all(|c| c.is_ascii_alphanumeric() || "/._-".contains(c))
            && !value.split('/').any(|part| part == "..");
        assert!(ok, "{name}={value:?} must be a plain absolute path without a trailing slash");
    }

    let src = format!(
        r#"/// Install prefix this build was made for (`VINOAUTHFACE_PREFIX`).
pub const PREFIX: &str = "{prefix}";
pub const BIN_DIR: &str = "{prefix}/bin";
pub const LIBEXEC_DIR: &str = "{libexec}";
/// Models, helper scripts and the SELinux sources.
pub const SHARE_DIR: &str = "{prefix}/share/face-auth";
/// OpenVINO's and the NPU compiler's root-owned prefixes live under here.
pub const LIB_DIR: &str = "{prefix}/lib/face-auth";

pub const FACE_AUTH: &str = "{prefix}/bin/vinoauthface-auth";
pub const FACE_ENROLL: &str = "{prefix}/bin/vinoauthface";
pub const TRAY: &str = "{prefix}/bin/vinoauthface-tray";
pub const SETTINGS_WINDOW: &str = "{prefix}/bin/vinoauthface-settings";
pub const UPGRADER: &str = "{prefix}/bin/vinoauthface-upgrade";
pub const HELPER: &str = "{libexec}/vinoauthface-helper";
pub const UNSEAL_HELPER: &str = "{libexec}/vinoauthface-unseal";
pub const LOGIN_MODE: &str = "{prefix}/share/face-auth/login-mode.sh";
pub const SETTING_MODE: &str = "{prefix}/share/face-auth/setting-mode.sh";
pub const DEFAULT_MODEL: &str = "{prefix}/share/face-auth/w600k_mbf.onnx";
pub const DEFAULT_DETECTOR: &str = "{prefix}/share/face-auth/det_500m.onnx";
pub const OPENVINO_DIR: &str = "{prefix}/lib/face-auth/openvino";
pub const OPENVINO_INTEL64_DIR: &str = "{prefix}/lib/face-auth/openvino/intel64";
pub const NPU_PREFIX_DIR: &str = "{prefix}/lib/face-auth/npu";
"#
    );
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("paths.rs");
    fs::write(out, src).expect("write paths.rs");
}
