//! Pare-feu Windows : sans règle d'entrée, les autres appareils ne peuvent ni répondre aux
//! annonces de ce PC ni s'y connecter, alors que lui les voit (cas typique d'un réseau classé
//! « Public » : la fenêtre de Windows ne coche que « Privé », et refuser crée des règles de blocage).
//!
//! Vérification sans droits administrateur (PowerShell), correction par une seule demande
//! d'élévation (UAC) : suppression des règles de blocage de MagicPlus, puis règles d'autorisation
//! pour le programme et pour ses deux ports, sur tous les profils réseau.

/// État du pare-feu tel que montré à l'interface.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum FirewallState {
    /// Rien à régler (autre système, pare-feu désactivé ou MagicPlus autorisé).
    #[default]
    Ok,
    /// Vérification en cours.
    Checking,
    /// MagicPlus n'est pas autorisé (ou est bloqué) : les autres appareils ne peuvent pas le joindre.
    Blocked,
    /// Correction en cours (fenêtre d'autorisation de Windows ouverte).
    Fixing,
    /// Échec de la correction (refus de l'élévation…), avec l'explication.
    Failed(String),
}

impl FirewallState {
    /// Vrai si l'interface doit proposer « Autoriser MagicPlus ».
    pub fn needs_action(&self) -> bool {
        matches!(self, FirewallState::Blocked | FirewallState::Failed(_))
    }
}

/// Nom des règles créées par MagicPlus.
pub const RULE_NAME: &str = "MagicPlus";

/// Chaîne PowerShell entre apostrophes.
pub fn ps_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// Script de vérification : affiche `ok` ou `bloque`.
pub fn check_script(exe: &str) -> String {
    format!(
        r#"$ErrorActionPreference = 'SilentlyContinue'
$exe = {exe}
if (@(Get-NetFirewallProfile | Where-Object {{ $_.Enabled -eq $true -or $_.Enabled -eq 'True' }}).Count -eq 0) {{ 'ok'; exit 0 }}
$mine = @(Get-NetFirewallApplicationFilter | Where-Object {{ $_.Program -ieq $exe }} | Get-NetFirewallRule | Where-Object {{ $_.Enabled -eq 'True' -and $_.Direction -eq 'Inbound' }})
$blocked = @($mine | Where-Object {{ $_.Action -eq 'Block' }}).Count
$allowed = @(Get-NetFirewallRule -DisplayName {rule} | Where-Object {{ $_.Enabled -eq 'True' -and $_.Direction -eq 'Inbound' -and $_.Action -eq 'Allow' }}).Count
# Règle acceptée dans la fenêtre de Windows : elle doit couvrir le type des réseaux actuels.
$cats = @(Get-NetConnectionProfile | ForEach-Object {{ if ("$($_.NetworkCategory)" -eq 'DomainAuthenticated') {{ 'Domain' }} else {{ "$($_.NetworkCategory)" }} }})
$covered = @($mine | Where-Object {{ $p = "$($_.Profile)"; $_.Action -eq 'Allow' -and ($p -eq 'Any' -or @($cats | Where-Object {{ $p -notmatch $_ }}).Count -eq 0) }}).Count
if ($blocked -eq 0 -and ($allowed -ge 3 -or $covered -ge 1)) {{ 'ok' }} else {{ 'bloque' }}
"#,
        exe = ps_quote(exe),
        rule = ps_quote(RULE_NAME),
    )
}

/// Script de correction (exécuté en administrateur).
pub fn allow_script(exe: &str, udp_port: u16, tcp_port: u16) -> String {
    format!(
        r#"$ErrorActionPreference = 'Stop'
$exe = {exe}
Get-NetFirewallApplicationFilter | Where-Object {{ $_.Program -ieq $exe }} | Get-NetFirewallRule | Where-Object {{ $_.Direction -eq 'Inbound' -and $_.Action -eq 'Block' }} | Remove-NetFirewallRule
Get-NetFirewallRule -DisplayName {rule} -ErrorAction SilentlyContinue | Remove-NetFirewallRule
New-NetFirewallRule -DisplayName {rule} -Description 'Appareils MagicPlus du réseau local' -Direction Inbound -Action Allow -Program $exe -Profile Any | Out-Null
New-NetFirewallRule -DisplayName {rule} -Description 'Annonces MagicPlus (UDP)' -Direction Inbound -Action Allow -Protocol UDP -LocalPort {udp_port} -Profile Any | Out-Null
New-NetFirewallRule -DisplayName {rule} -Description 'Connexions MagicPlus (TCP)' -Direction Inbound -Action Allow -Protocol TCP -LocalPort {tcp_port} -Profile Any | Out-Null
"#,
        exe = ps_quote(exe),
        rule = ps_quote(RULE_NAME),
    )
}

/// Script encodé pour `powershell -EncodedCommand` (UTF-16LE en base64).
pub fn encode_script(script: &str) -> String {
    use base64::Engine as _;
    let bytes: Vec<u8> = script.encode_utf16().flat_map(|unit| unit.to_le_bytes()).collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[cfg(target_os = "windows")]
mod system {
    use super::*;
    use anyhow::{anyhow, bail, Result};
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};

    fn powershell() -> Command {
        let mut command = Command::new("powershell.exe");
        // Pas de fenêtre de console.
        command.creation_flags(0x0800_0000);
        command.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass"]).stdin(Stdio::null());
        command
    }

    fn current_exe() -> Result<String> {
        let exe = std::env::current_exe().map_err(|error| anyhow!("chemin du programme inconnu : {error}"))?;
        Ok(exe.to_string_lossy().trim_start_matches(r"\\?\").to_string())
    }

    pub fn check() -> FirewallState {
        let Ok(exe) = current_exe() else { return FirewallState::Ok };
        let output = powershell().args(["-EncodedCommand", &encode_script(&check_script(&exe))]).output();
        match output {
            Ok(output) if String::from_utf8_lossy(&output.stdout).contains("bloque") => FirewallState::Blocked,
            // PowerShell absent ou en erreur : on ne dérange pas l'utilisateur pour rien.
            _ => FirewallState::Ok,
        }
    }

    pub fn allow(udp_port: u16, tcp_port: u16) -> Result<()> {
        let exe = current_exe()?;
        let inner = encode_script(&allow_script(&exe, udp_port, tcp_port));
        // Une seule demande d'élévation ; le code de sortie de la fenêtre élevée est renvoyé.
        let outer = format!(
            "try {{ $p = Start-Process powershell.exe -Verb RunAs -WindowStyle Hidden -Wait -PassThru \
             -ArgumentList '-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-EncodedCommand','{inner}'; exit $p.ExitCode }} \
             catch {{ exit 1223 }}"
        );
        let status = powershell()
            .args(["-EncodedCommand", &encode_script(&outer)])
            .status()
            .map_err(|error| anyhow!("PowerShell introuvable : {error}"))?;
        match status.code() {
            Some(0) => Ok(()),
            Some(1223) => bail!("autorisation refusée dans la fenêtre de Windows"),
            Some(code) => bail!("Windows a refusé la règle de pare-feu (code {code})"),
            None => bail!("réglage du pare-feu interrompu"),
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod system {
    use super::*;

    pub fn check() -> FirewallState {
        FirewallState::Ok
    }

    pub fn allow(_udp_port: u16, _tcp_port: u16) -> anyhow::Result<()> {
        Ok(())
    }
}

/// Vérifie le pare-feu (lent : quelques secondes sous Windows, à lancer hors de l'interface).
pub fn check() -> FirewallState {
    system::check()
}

/// Autorise MagicPlus (demande d'élévation sous Windows).
pub fn allow(udp_port: u16, tcp_port: u16) -> anyhow::Result<()> {
    system::allow(udp_port, tcp_port)
}

/// Vrai sur les systèmes où MagicPlus sait régler le pare-feu.
pub const SUPPORTED: bool = cfg!(target_os = "windows");

// ------------------------------------------------------------------ cartes réseau virtuelles

/// Carte réseau virtuelle (machines virtuelles, conteneurs, VPN) d'après son nom : son adresse
/// n'est joignable que depuis cet ordinateur, inutile de la donner aux autres appareils.
pub fn is_virtual_adapter(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    const MARKERS: [&str; 18] = [
        "vmware",
        "vmnet",
        "virtualbox",
        "vboxnet",
        "vethernet",
        "hyper-v",
        "wsl",
        "docker",
        "veth",
        "virbr",
        "bridge10",
        "utun",
        "parallels",
        "vnic",
        "zerotier",
        "tailscale",
        "hamachi",
        "loopback",
    ];
    MARKERS.iter().any(|marker| name.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_adapters_are_recognised() {
        for name in [
            "VMware Network Adapter VMnet1",
            "VMware Network Adapter VMnet8",
            "vEthernet (WSL)",
            "vEthernet (Default Switch)",
            "VirtualBox Host-Only Network",
            "vmnet8",
            "bridge100",
            "docker0",
        ] {
            assert!(is_virtual_adapter(name), "{name}");
        }
        for name in ["Wi-Fi", "Ethernet", "en0", "en1", "Ethernet 2", "wlan0", "eth0"] {
            assert!(!is_virtual_adapter(name), "{name}");
        }
    }

    #[test]
    fn scripts_quote_paths_and_encode() {
        let exe = r"C:\Users\O'Neil\MagicPlus.exe";
        let check = check_script(exe);
        assert!(check.contains(r"'C:\Users\O''Neil\MagicPlus.exe'"));
        let allow = allow_script(exe, 45454, 45455);
        assert!(allow.contains("-LocalPort 45454") && allow.contains("-LocalPort 45455"));
        assert!(allow.contains("-Profile Any"));
        // UTF-16LE : « a » → 61 00 → « YQA= ».
        assert_eq!(encode_script("a"), "YQA=");
    }
}
