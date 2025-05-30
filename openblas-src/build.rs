use std::{env, path::*, process::Command};

#[allow(unused)]
fn run(command: &mut Command) {
    println!("Running: `{:?}`", command);
    match command.status() {
        Ok(status) => {
            if !status.success() {
                panic!("Failed: `{:?}` ({})", command, status);
            }
        }
        Err(error) => {
            panic!("Failed: `{:?}` ({})", command, error);
        }
    }
}

fn feature_enabled(feature: &str) -> bool {
    env::var(format!("CARGO_FEATURE_{}", feature.to_uppercase())).is_ok()
}

/// Add path where pacman (on msys2) install OpenBLAS
///
/// - `pacman -S mingw-w64-x86_64-openblas` will install
///   - `libopenbla.dll` into `/mingw64/bin`
///   - `libopenbla.a`   into `/mingw64/lib`
/// - But we have to specify them using `-L` in **Windows manner**
///   - msys2 `/` is `C:\msys64\` in Windows by default install
///   - It can be convert using `cygpath` command
fn windows_gnu_system() {
    let lib_path = String::from_utf8(
        Command::new("cygpath")
            .arg("-w")
            .arg(if feature_enabled("static") {
                "/mingw64/bin"
            } else {
                "/mingw64/lib"
            })
            .output()
            .expect("Failed to exec cygpath")
            .stdout,
    )
    .expect("cygpath output includes non UTF-8 string");
    println!("cargo:rustc-link-search={}", lib_path);
}

/// Use vcpkg for msvc "system" feature
fn windows_msvc_system() {
    if !feature_enabled("static") {
        env::set_var("VCPKGRS_DYNAMIC", "1");
    }
    #[cfg(target_env = "msvc")]
    vcpkg::find_package("openblas").expect(
        "vcpkg failed to find OpenBLAS package , Try to install it using `vcpkg install openblas:$(ARCH)-windows(-static)(-md)`"
    );
    if !cfg!(target_env = "msvc") {
        unreachable!();
    }
}

/// Add linker flag (`-L`) to path where brew installs OpenBLAS
fn macos_system() {
    fn brew_prefix(target: &str) -> PathBuf {
        let out = Command::new("brew")
            .arg("--prefix")
            .arg(target)
            .output()
            .expect("brew not installed");
        assert!(out.status.success(), "`brew --prefix` failed");
        let path = String::from_utf8(out.stdout).expect("Non-UTF8 path by `brew --prefix`");
        PathBuf::from(path.trim())
    }
    let openblas = brew_prefix("openblas");
    let libomp = brew_prefix("libomp");

    println!("cargo:rustc-link-search={}/lib", openblas.display());
    println!("cargo:rustc-link-search={}/lib", libomp.display());
}

fn main() {
    if env::var("DOCS_RS").is_ok() {
        return;
    }
    let link_kind = if feature_enabled("static") {
        "static"
    } else {
        "dylib"
    };
    if feature_enabled("system") {
        // Use pkg-config to find OpenBLAS
        if pkg_config::Config::new()
            .statik(feature_enabled("static"))
            .probe("openblas")
            .inspect_err(|e| eprintln!("PKG_CONFIG err: {e:?}"))
            .is_ok()
        {
            return;
        }

        if cfg!(target_os = "windows") {
            if cfg!(target_env = "gnu") {
                windows_gnu_system();
            } else if cfg!(target_env = "msvc") {
                windows_msvc_system();
            } else {
                panic!(
                    "Unsupported ABI for Windows: {}",
                    env::var("CARGO_CFG_TARGET_ENV").unwrap()
                );
            }
        }
        if cfg!(target_os = "macos") {
            macos_system();
        }
        println!("cargo:rustc-link-lib={}=openblas", link_kind);
    } else {
        if cfg!(target_env = "msvc") {
            panic!(
                "Non-vcpkg builds are not supported on Windows. You must use the 'system' feature."
            )
        }
        build();
    }
    println!("cargo:rustc-link-lib={}=openblas", link_kind);
}

/// Build OpenBLAS using openblas-build crate
fn build() {
    println!("cargo:rerun-if-env-changed=OPENBLAS_TARGET");
    println!("cargo:rerun-if-env-changed=OPENBLAS_CC");
    println!("cargo:rerun-if-env-changed=OPENBLAS_HOSTCC");
    println!("cargo:rerun-if-env-changed=OPENBLAS_FC");
    println!("cargo:rerun-if-env-changed=OPENBLAS_RANLIB");
    let mut cfg = openblas_build::Configure::default();
    if !feature_enabled("cblas") {
        cfg.no_cblas = true;
    }
    if !feature_enabled("lapacke") {
        cfg.no_lapacke = true;
    }
    if feature_enabled("static") {
        cfg.no_shared = true;
    } else {
        cfg.no_static = true;
    }
    if let Ok(target) = env::var("OPENBLAS_TARGET") {
        cfg.target = Some(
            target
                .parse()
                .expect("Unsupported target is specified by $OPENBLAS_TARGET"),
        )
        // Do not default to the native target (represented by `cfg.target == None`)
        // because most user set `$OPENBLAS_TARGET` explicitly will hope not to use the native target.
    }
    cfg.compilers.cc = env::var("OPENBLAS_CC").ok();
    cfg.compilers.hostcc = env::var("OPENBLAS_HOSTCC").ok();
    cfg.compilers.fc = env::var("OPENBLAS_FC").ok();
    cfg.compilers.ranlib = env::var("OPENBLAS_RANLIB").ok();

    let output = if feature_enabled("cache") {
        use std::{
            collections::hash_map::DefaultHasher,
            hash::{Hash, Hasher},
        };
        // Build OpenBLAS on user's data directory.
        // See https://docs.rs/dirs/5.0.1/dirs/fn.data_dir.html
        //
        // On Linux, `data_dir` returns `$XDG_DATA_HOME` or `$HOME/.local/share`.
        // This build script creates a directory based on the hash value of `cfg`,
        // i.e. `$XDG_DATA_HOME/openblas_build/[Hash of cfg]`, and build OpenBLAS there.
        //
        // This build will be shared among several projects using openblas-src crate.
        // It makes users not to build OpenBLAS in every `cargo build`.
        let mut hasher = DefaultHasher::new();
        cfg.hash(&mut hasher);

        dirs::data_dir()
            .expect("Cannot get user's data directory")
            .join("openblas_build")
            .join(format!("{:x}", hasher.finish()))
    } else {
        PathBuf::from(env::var("OUT_DIR").unwrap())
    };
    let source = openblas_build::download(&output).unwrap();

    // If OpenBLAS is build as shared, user of openblas-src will have to find `libopenblas.so` at runtime.
    //
    // `cargo run` appends the link paths to `LD_LIBRARY_PATH` specified by `cargo:rustc-link-search`,
    // and user's crate can find it then.
    //
    // However, when user try to run it directly like `./target/release/user_crate_exe`, it will say
    // "error while loading shared libraries: libopenblas.so: cannot open shared object file: No such file or directory".
    //
    // Be sure that `cargo:warning` is shown only when openblas-src is build as path dependency...
    // https://doc.rust-lang.org/cargo/reference/build-scripts.html#cargowarningmessage
    if !feature_enabled("static") {
        let ld_name = if cfg!(target_os = "macos") {
            "DYLD_LIBRARY_PATH"
        } else {
            "LD_LIBRARY_PATH"
        };
        println!(
            "cargo:warning=OpenBLAS is built as a shared library. You need to set {}={}",
            ld_name,
            source.display()
        );
    }

    let build_result = cfg.build(&source);
    let make_conf = match build_result {
        Ok(c) => c,
        Err(openblas_build::error::Error::MissingCrossCompileInfo { info }) => {
            panic!(
                "Cross compile information is missing and cannot be inferred: OPENBLAS_{}",
                info
            );
        }
        Err(e) => {
            panic!("OpenBLAS build failed: {}", e);
        }
    };

    println!("cargo:rustc-link-search={}", source.display());
    for search_path in &make_conf.c_extra_libs.search_paths {
        println!("cargo:rustc-link-search={}", search_path.display());
    }
    for lib in &make_conf.c_extra_libs.libs {
        println!("cargo:rustc-link-lib={}", lib);
    }
    for search_path in &make_conf.f_extra_libs.search_paths {
        println!("cargo:rustc-link-search={}", search_path.display());
    }
    for lib in &make_conf.f_extra_libs.libs {
        println!("cargo:rustc-link-lib={}", lib);
    }
}
