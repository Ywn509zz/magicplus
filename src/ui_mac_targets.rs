//! Choix des appareils qui reçoivent un fichier ou le presse-papiers (Mac).

use super::*;

impl Controller {
    /// Demande à quels appareils envoyer (cases cochées : tous). `None` si l'utilisateur annule ;
    /// `Some(None)` : tous (un seul appareil joignable, ou aucun : le moteur l'explique).
    pub(super) fn choose_targets(&self, what: &str) -> Option<Option<Vec<String>>> {
        let devices = self.engine().reachable_devices();
        if devices.len() <= 1 {
            return Some(None);
        }
        let mtm = self.mtm();
        let column = stack(true, 6.0, mtm);
        let boxes: Vec<(String, Retained<NSButton>)> = devices
            .into_iter()
            .map(|(id, name)| {
                let check = unsafe { NSButton::checkboxWithTitle_target_action(&ns(&name), None, None, mtm) };
                check.setState(NSControlStateValueOn);
                column.addArrangedSubview(&check);
                (id, check)
            })
            .collect();
        column.setFrameSize(column.fittingSize());
        let alert = NSAlert::new(mtm);
        alert.setMessageText(&ns(&format!("Envoyer {what} à…")));
        alert.setInformativeText(&ns("Choisissez les appareils qui le reçoivent."));
        alert.setAccessoryView(Some(&column));
        alert.addButtonWithTitle(&ns("Envoyer"));
        alert.addButtonWithTitle(&ns("Annuler"));
        if alert.runModal() != NSAlertFirstButtonReturn {
            return None;
        }
        let chosen: Vec<String> = boxes.into_iter().filter(|(_, check)| check.state() == NSControlStateValueOn).map(|(id, _)| id).collect();
        if chosen.is_empty() {
            self.engine().set_status("Aucun appareil choisi : rien n'a été envoyé");
            return None;
        }
        Some(Some(chosen))
    }
}
