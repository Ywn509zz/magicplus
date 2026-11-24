//! Nouveautés de chaque version (`CHANGELOG.md`, intégré au programme) : affichées une fois après
//! une mise à jour, et à la demande.

use crate::engine::{lock, Engine};

const CHANGELOG: &str = include_str!("../CHANGELOG.md");

/// Une version et ses nouveautés (Markdown).
#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub version: String,
    pub notes: String,
}

/// Nouveautés à montrer après une mise à jour.
#[derive(Debug, Clone, PartialEq)]
pub struct WhatsNew {
    /// Version d'avant (celle du dernier lancement).
    pub from: String,
    pub to: String,
    /// Sections des versions installées depuis, la plus récente d'abord (Markdown).
    pub notes: String,
}

/// Toutes les versions du fichier, la plus récente d'abord.
pub fn releases() -> Vec<Release> {
    parse(CHANGELOG)
}

fn parse(text: &str) -> Vec<Release> {
    let mut list: Vec<Release> = Vec::new();
    for line in text.lines() {
        if let Some(version) = line.strip_prefix("## ") {
            list.push(Release { version: version.trim().to_string(), notes: String::new() });
        } else if let Some(current) = list.last_mut() {
            current.notes.push_str(line);
            current.notes.push('\n');
        }
    }
    for release in &mut list {
        release.notes = release.notes.trim().to_string();
    }
    list
}

fn key(version: &str) -> Vec<u64> {
    version.split(['.', '-']).map(|part| part.parse().unwrap_or(0)).collect()
}

/// Sections des versions comprises entre `from` (exclue) et `to` (incluse).
fn notes_between(releases: &[Release], from: &str, to: &str) -> String {
    releases
        .iter()
        .filter(|r| key(&r.version) > key(from) && key(&r.version) <= key(to))
        .map(|r| format!("## Version {}\n{}", r.version, r.notes))
        .collect::<Vec<_>>()
        .join("\n\n")
}

impl Engine {
    /// Nouveautés depuis le dernier lancement, s'il s'agit d'une mise à jour (rien au premier
    /// lancement ni quand la version n'a pas changé). `dismiss_whats_new` les marque comme vues.
    pub fn whats_new(&self) -> Option<WhatsNew> {
        let current = env!("CARGO_PKG_VERSION");
        let mut previous = lock(&self.inner.settings).last_version.clone();
        if previous.is_empty() {
            if self.inner.fresh_install {
                return None;
            }
            // Mise à jour depuis une version qui ne retenait pas sa version : la précédente du fichier.
            previous = releases().get(1).map(|r| r.version.clone()).unwrap_or_default();
        }
        if previous.is_empty() || key(&previous) >= key(current) {
            return None;
        }
        let notes = notes_between(&releases(), &previous, current);
        (!notes.is_empty()).then(|| WhatsNew { from: previous, to: current.to_string(), notes })
    }

    /// Retient la version lancée (au premier lancement, et une fois les nouveautés vues).
    pub fn dismiss_whats_new(&self) {
        let current = env!("CARGO_PKG_VERSION").to_string();
        if lock(&self.inner.settings).last_version != current {
            let _ = self.update_settings(|settings| settings.last_version = current);
        }
    }

    /// Nouveautés de la version installée (bouton « Voir les nouveautés »).
    pub fn current_release_notes(&self) -> String {
        let current = env!("CARGO_PKG_VERSION");
        releases()
            .into_iter()
            .take(3)
            .map(|r| {
                let title = if r.version == current { format!("## Version {} (installée)", r.version) } else { format!("## Version {}", r.version) };
                format!("{title}\n{}", r.notes)
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changelog_has_current_version_and_ranges() {
        let all = releases();
        assert_eq!(all[0].version, env!("CARGO_PKG_VERSION"), "CHANGELOG.md doit commencer par la version de Cargo.toml");
        assert!(all.iter().all(|r| !r.notes.is_empty()));
        let sample = parse("intro\n## 1.2.0\n- c\n## 1.1.0\n- b\n## 1.0.0\n- a\n");
        let notes = notes_between(&sample, "1.0.0", "1.2.0");
        assert!(notes.contains("Version 1.2.0") && notes.contains("Version 1.1.0") && !notes.contains("1.0.0"));
        assert!(notes.find("1.2.0") < notes.find("1.1.0"));
        assert!(notes_between(&sample, "1.2.0", "1.2.0").is_empty());
        assert!(key("0.10.0") > key("0.9.0"));
    }
}
