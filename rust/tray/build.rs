// Ресурсы CoreMapTray.exe: значок (тот же, что у CoreMap.exe) и манифест (обычные права с самоповышением, версии
// Windows, масштаб экрана). Собираются компилятором ресурсов из Windows SDK (rc.exe ставится с Build Tools) без
// сторонних библиотек: ни одной новой загрузки ради значка (DECISIONS #31). Версия и метка сборки — как у окна
// (tools\rust.ps1): номер версии для владельца, метка — чтобы файл был другим при пересборке.
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=COREMAP_STAMP");
    println!("cargo:rerun-if-env-changed=COREMAP_VERSION");
    let stamp = std::env::var("COREMAP_STAMP").unwrap_or_default();
    println!("cargo:rustc-env=COREMAP_BUILT={stamp}");
    let version = std::env::var("COREMAP_VERSION").unwrap_or_else(|_| "без номера".into());
    println!("cargo:rustc-env=COREMAP_VERSION={version}");
    for file in ["tray.rc", "tray.manifest", "../../assets/coremap.ico"] {
        println!("cargo:rerun-if-changed={file}");
    }
    let rc = compiler().expect("rc.exe from the Windows SDK (Build Tools) is needed to build the tray icon resource");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR")).join("tray.res");
    let status = Command::new(&rc)
        .args(["/nologo", "/fo"])
        .arg(&out)
        .arg("tray.rc")
        .status()
        .expect("rc.exe starts");
    assert!(status.success(), "rc.exe failed on tray.rc");
    // link.exe принимает .res как есть и переводит в объектный файл сам.
    println!("cargo:rustc-link-arg-bins={}", out.display());
}

/// rc.exe самой новой версии Windows SDK для x64.
fn compiler() -> Option<PathBuf> {
    let base = std::env::var_os("ProgramFiles(x86)")
        .map(PathBuf::from)?
        .join("Windows Kits")
        .join("10")
        .join("bin");
    let mut versions: Vec<PathBuf> = std::fs::read_dir(&base)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("x64").join("rc.exe"))
        .filter(|rc| rc.is_file())
        .collect();
    versions.sort();
    versions.pop()
}
