use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

pub fn platform_name() -> &'static str {
    match std::env::consts::OS {
        "macos" => "macos",
        "windows" => "windows",
        "linux" => "linux",
        _ => "desktop",
    }
}

/// Nom lisible de la machine, utilisé comme nom d'appareil par défaut.
pub fn default_device_name() -> String {
    let from_command = |program: &str, args: &[&str]| -> Option<String> {
        let output = Command::new(program).args(args).output().ok()?;
        let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
        (output.status.success() && !name.is_empty()).then_some(name)
    };

    let name = match std::env::consts::OS {
        "macos" => from_command("scutil", &["--get", "ComputerName"]),
        "windows" => std::env::var("COMPUTERNAME").ok().filter(|name| !name.is_empty()),
        _ => None,
    }
    .or_else(|| from_command("hostname", &[]));

    name.unwrap_or_else(|| format!("MagicPlus {}", platform_name()))
}

pub fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let value = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} o")
    } else if value < KB * KB {
        format!("{:.0} Ko", value / KB)
    } else if value < KB * KB * KB {
        format!("{:.1} Mo", value / KB / KB)
    } else {
        format!("{:.2} Go", value / KB / KB / KB)
    }
}

/// Garde uniquement un nom de fichier sûr : pas de dossier, pas de caractères interdits sous Windows.
pub fn sanitize_file_name(name: &str) -> String {
    let base = Path::new(&name.replace('\\', "/"))
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_string();
    let cleaned: String = base
        .chars()
        .map(|character| {
            if character.is_control() || "<>:\"/\\|?*".contains(character) {
                '_'
            } else {
                character
            }
        })
        .collect();
    let cleaned = cleaned.trim().trim_end_matches('.').to_string();
    if cleaned.is_empty() || cleaned == ".." {
        "magicplus-fichier".to_string()
    } else {
        cleaned
    }
}

/// `nom.ext`, puis `nom-1.ext`, `nom-2.ext`… si le fichier existe déjà.
pub fn unique_destination(folder: &Path, file_name: &str) -> PathBuf {
    let original = folder.join(file_name);
    if !original.exists() {
        return original;
    }

    let path = Path::new(file_name);
    let stem = path.file_stem().and_then(|value| value.to_str()).unwrap_or("fichier");
    let extension = path.extension().and_then(|value| value.to_str()).unwrap_or("");

    for index in 1..10_000 {
        let candidate = if extension.is_empty() {
            format!("{stem}-{index}")
        } else {
            format!("{stem}-{index}.{extension}")
        };
        let candidate = folder.join(candidate);
        if !candidate.exists() {
            return candidate;
        }
    }
    folder.join(format!("{stem}-{}", now_ms()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_hostile_names() {
        assert_eq!(sanitize_file_name("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_file_name("C:\\Windows\\evil.exe"), "evil.exe");
        assert_eq!(sanitize_file_name("a<b>c.txt"), "a_b_c.txt");
        assert_eq!(sanitize_file_name(".."), "magicplus-fichier");
        assert_eq!(sanitize_file_name(""), "magicplus-fichier");
    }

    #[test]
    fn formats_sizes() {
        assert_eq!(format_size(12), "12 o");
        assert_eq!(format_size(2048), "2 Ko");
        assert_eq!(format_size(5 * 1024 * 1024), "5.0 Mo");
    }
}
