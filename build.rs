use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=SystemRoot");

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os != "windows" || target_env != "gnu" {
        return;
    }

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR missing"));
    let system_root = env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
    let opencl_dll = PathBuf::from(system_root)
        .join("System32")
        .join("OpenCL.dll");

    if !opencl_dll.exists() {
        return;
    }

    let gendef_status = Command::new("gendef")
        .arg(&opencl_dll)
        .current_dir(&out_dir)
        .status();

    if !matches!(gendef_status, Ok(status) if status.success()) {
        return;
    }

    let def_path = out_dir.join("OpenCL.def");
    let lib_path = out_dir.join("libOpenCL.a");
    let dlltool_status = Command::new("dlltool")
        .arg("-d")
        .arg(&def_path)
        .arg("-l")
        .arg(&lib_path)
        .arg("-D")
        .arg("OpenCL.dll")
        .status();

    if matches!(dlltool_status, Ok(status) if status.success()) {
        println!("cargo:rustc-link-search=native={}", out_dir.display());
    }
}
