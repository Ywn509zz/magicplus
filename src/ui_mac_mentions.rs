//! Mentions « @nom » sur Mac : suggestions sous la zone de saisie pendant la frappe (Entrée ou
//! Tab : la première, Échap : fermer), mise en évidence dans les messages.

use super::menus::Entry;
use super::*;
use magicplus_core::mentions;
use objc2_app_kit::{NSPopoverBehavior, NSTextInputClient};
use objc2_foundation::NSRange;

/// Octets ↔ unités UTF-16 (positions de NSTextView).
fn byte_index(text: &str, utf16: usize) -> usize {
    let mut units = 0;
    for (index, c) in text.char_indices() {
        if units >= utf16 {
            return index;
        }
        units += c.len_utf16();
    }
    text.len()
}

fn utf16_index(text: &str, byte: usize) -> usize {
    text[..byte.min(text.len())].encode_utf16().count()
}

/// Mention en cours : début du « @ » et curseur (UTF-16), et le nom de la première suggestion.
#[derive(Clone)]
pub(super) struct PendingMention {
    at: usize,
    cursor: usize,
    first: String,
}

impl Controller {
    /// Texte modifié : met à jour les suggestions de mention.
    pub(super) fn mention_text_changed(&self) {
        let Some(conversation) = self.current_conversation_id() else { return };
        let input = self.chat_input();
        let text = input.string().to_string();
        let cursor = byte_index(&text, input.selectedRange().location);
        let found = mentions::pending_query(&text, cursor)
            .map(|(at, query)| (at, self.engine().mention_choices(&conversation, &query)))
            .filter(|(_, choices)| !choices.is_empty());
        let Some((at, choices)) = found else {
            self.close_mentions();
            return;
        };
        let at16 = utf16_index(&text, at);
        let cursor16 = utf16_index(&text, cursor);
        *self.ivars().mention.borrow_mut() = Some(PendingMention { at: at16, cursor: cursor16, first: choices[0].name.clone() });
        let mut entries = vec![Entry::Note("MEMBRES — Entrée ou Tab pour choisir".to_string())];
        for choice in choices {
            let name = choice.name.clone();
            let title = if choice.name != choice.label {
                format!("{}   @{}", choice.label, choice.name)
            } else if !choice.detail.is_empty() {
                format!("{}   {}", choice.label, choice.detail)
            } else {
                choice.label.clone()
            };
            let symbol = if choice.id.is_empty() { "person.3" } else { "at" };
            entries.push(Entry::Item(symbol, title, false, Box::new(move |c: &Controller| c.insert_mention(&name))));
        }
        let anchor: Retained<NSView> = Retained::into_super(Retained::into_super(input.clone()));
        self.show_actions(&anchor, entries);
        // La bulle ne se ferme pas toute seule et la frappe continue dans la zone de saisie.
        if let Some(popover) = self.ivars().popover.borrow().as_ref() {
            popover.setBehavior(NSPopoverBehavior::ApplicationDefined);
        }
        if let Some(window) = input.window() {
            window.makeFirstResponder(Some(&input));
        }
    }

    /// Remplace « @requête » par « @nom ».
    pub(super) fn insert_mention(&self, name: &str) {
        let Some(pending) = self.ivars().mention.borrow_mut().take() else { return };
        let input = self.chat_input();
        // Jamais hors du texte (sinon AppKit lève une exception).
        let length = input.string().to_string().encode_utf16().count();
        let (at, cursor) = (pending.at.min(length), pending.cursor.min(length));
        let range = NSRange::new(at, cursor.saturating_sub(at));
        // SAFETY : chaîne valable, plage dans le texte (relevée à la dernière frappe).
        unsafe { input.insertText_replacementRange(&ns(&format!("@{name} ")), range) };
        self.close_mentions();
        if let Some(window) = input.window() {
            window.makeFirstResponder(Some(&input));
        }
    }

    /// Entrée ou Tab pendant les suggestions : la première ; Échap : fermer. Vrai si traité.
    pub(super) fn mention_command(&self, selector: Sel) -> bool {
        let first = self.ivars().mention.borrow().as_ref().map(|p| p.first.clone());
        let Some(first) = first else { return false };
        if selector == sel!(insertNewline:) || selector == sel!(insertTab:) {
            self.insert_mention(&first);
            return true;
        }
        if selector == sel!(cancelOperation:) {
            self.close_mentions();
            return true;
        }
        false
    }

    pub(super) fn close_mentions(&self) {
        if self.ivars().mention.borrow_mut().take().is_some() {
            self.close_popover();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_positions() {
        let text = "é😀@a";
        assert_eq!(utf16_index(text, text.find('@').unwrap()), 3);
        assert_eq!(byte_index(text, 3), text.find('@').unwrap());
        assert_eq!(byte_index(text, 99), text.len());
    }
}
