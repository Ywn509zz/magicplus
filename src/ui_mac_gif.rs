//! GIF de GIPHY sur Mac : bulle surgissante avec recherche, tendances et favoris. Les
//! résultats et leurs aperçus arrivent d'un fil de travail ; la bulle est refaite à leur arrivée.
//! Clic : envoyer ; ⌥-clic : ajouter aux favoris ou en retirer.

use super::menus::Action;
use super::*;
use magicplus_core::GifResult;
use objc2::{AnyThread, Message};
use objc2_app_kit::{NSBezelStyle, NSClickGestureRecognizer};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

/// Résultat d'une recherche, prêt à afficher : le GIF et son aperçu (fichier).
type Found = Vec<(GifResult, PathBuf)>;
/// Réponses des fils de travail : (numéro de recherche, résultats ou erreur).
type Inbox = Arc<Mutex<Vec<(u64, Result<Found, String>)>>>;

#[derive(Default)]
pub(super) struct GifUi {
    generation: u64,
    favorites_tab: bool,
    query: String,
    anchor: Option<Retained<NSView>>,
    results: Found,
    status: String,
    inbox: Inbox,
    field: Option<Retained<NSTextField>>,
}

const COLUMNS: usize = 3;
const TILE: f64 = 74.0;

impl Controller {
    /// Bouton « GIF » de la zone de saisie.
    pub(super) fn gif_button(&self, anchor: &NSView) {
        {
            let mut gif = self.ivars().gif.borrow_mut();
            gif.anchor = Some(anchor.retain());
        }
        if self.engine().giphy_key().is_none() {
            self.gif_key_popover();
        } else {
            self.gif_search();
        }
    }

    /// Lance la recherche en cours (tendances, `query`, ou favoris) dans un fil de travail.
    fn gif_search(&self) {
        let engine = self.engine().clone();
        let (generation, favorites_tab, query, inbox) = {
            let mut gif = self.ivars().gif.borrow_mut();
            gif.generation += 1;
            gif.status = if gif.favorites_tab { "Favoris…".into() } else { "Recherche…".into() };
            (gif.generation, gif.favorites_tab, gif.query.clone(), gif.inbox.clone())
        };
        self.gif_popover();
        std::thread::spawn(move || {
            let results = if favorites_tab { Ok(engine.gif_favorites()) } else { engine.gif_search_or_offline(&query, 0) };
            let found = results
                .map(|results| results.into_iter().filter_map(|gif| engine.giphy_preview(&gif).ok().map(|path| (gif, path))).collect())
                .map_err(|error| error.to_string());
            if let Ok(mut inbox) = inbox.lock() {
                inbox.push((generation, found));
            }
        });
    }

    /// Appelé par le minuteur : affiche les résultats arrivés pour la recherche en cours.
    pub(super) fn gif_tick(&self) {
        let news = {
            let gif = self.ivars().gif.borrow();
            let Ok(mut inbox) = gif.inbox.lock() else { return };
            std::mem::take(&mut *inbox)
        };
        let mut changed = false;
        for (generation, found) in news {
            let mut gif = self.ivars().gif.borrow_mut();
            if generation != gif.generation {
                continue;
            }
            match found {
                Ok(results) => {
                    gif.status = match (results.is_empty(), gif.favorites_tab) {
                        (true, true) => "Aucun favori : ⌥-clic sur un GIF pour l'ajouter".into(),
                        (true, false) => "Aucun GIF trouvé".into(),
                        _ => "Clic : envoyer · ⌥-clic : favori".into(),
                    };
                    gif.results = results;
                }
                Err(error) => gif.status = error,
            }
            changed = true;
        }
        let open = self.ivars().popover.borrow().as_ref().is_some_and(|p| p.isShown());
        if changed && open {
            self.gif_popover();
        }
    }

    /// Bulle de GIF : champ de recherche, onglets, grille d'aperçus animés.
    fn gif_popover(&self) {
        let mtm = self.mtm();
        let Some(anchor) = self.ivars().gif.borrow().anchor.clone() else { return };
        let mut actions: Vec<Action> = Vec::new();
        let column = stack(true, 6.0, mtm);
        column.setEdgeInsets(NSEdgeInsets { top: 10.0, left: 8.0, bottom: 10.0, right: 8.0 });

        let (query, favorites_tab, status, results) = {
            let gif = self.ivars().gif.borrow();
            (gif.query.clone(), gif.favorites_tab, gif.status.clone(), gif.results.clone())
        };

        let search = stack(false, 6.0, mtm);
        let field = NSTextField::textFieldWithString(&ns(&query), mtm);
        field.setPlaceholderString(Some(&ns("Rechercher sur GIPHY")));
        field.setTranslatesAutoresizingMaskIntoConstraints(false);
        field.widthAnchor().constraintEqualToConstant(160.0).setActive(true);
        search.addArrangedSubview(&field);
        self.ivars().gif.borrow_mut().field = Some(field.clone());
        let tag = actions.len() as isize;
        actions.push(Box::new(|c: &Controller| {
            let typed = c.ivars().gif.borrow().field.as_ref().map(|f| f.stringValue().to_string()).unwrap_or_default();
            {
                let mut gif = c.ivars().gif.borrow_mut();
                gif.query = typed.trim().to_string();
                gif.favorites_tab = false;
            }
            c.gif_search();
        }));
        let go = button("Chercher", sel!(popoverAction:), self, mtm);
        go.setTag(tag);
        search.addArrangedSubview(&go);
        column.addArrangedSubview(&search);

        let tabs = stack(false, 4.0, mtm);
        for (index, title) in [if query.is_empty() { "🔥 Tendances" } else { "🔎 Résultats" }, "⭐ Favoris"].into_iter().enumerate() {
            let tag = actions.len() as isize;
            let favorites = index == 1;
            actions.push(Box::new(move |c: &Controller| {
                c.ivars().gif.borrow_mut().favorites_tab = favorites;
                c.gif_search();
            }));
            let tab = button(title, sel!(popoverAction:), self, mtm);
            tab.setTag(tag);
            tab.setBezelStyle(NSBezelStyle::AccessoryBar);
            if favorites == favorites_tab {
                tab.setFont(Some(&semibold(12.0)));
            }
            tabs.addArrangedSubview(&tab);
        }
        column.addArrangedSubview(&tabs);

        for chunk in results.chunks(COLUMNS) {
            let row = stack(false, 4.0, mtm);
            for (gif, path) in chunk {
                let Some(image) = NSImage::initWithContentsOfFile(NSImage::alloc(), &ns(&path.to_string_lossy())) else { continue };
                let view = NSImageView::imageViewWithImage(&image, mtm);
                view.setAnimates(true);
                view.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
                view.setTranslatesAutoresizingMaskIntoConstraints(false);
                view.widthAnchor().constraintEqualToConstant(TILE).setActive(true);
                view.heightAnchor().constraintEqualToConstant(TILE * 0.8).setActive(true);
                let favorite = self.engine().is_gif_favorite(&gif.id);
                view.setToolTip(Some(&ns(&format!("{}{}", if favorite { "★ " } else { "" }, gif.title))));
                let tag = actions.len() as isize;
                let chosen = gif.clone();
                actions.push(Box::new(move |c: &Controller| c.gif_chosen(&chosen)));
                view.setTag(tag);
                let click = unsafe {
                    NSClickGestureRecognizer::initWithTarget_action(NSClickGestureRecognizer::alloc(mtm), Some(as_target(self)), Some(sel!(gifTileClicked:)))
                };
                view.addGestureRecognizer(&click);
                row.addArrangedSubview(&view);
            }
            column.addArrangedSubview(&row);
        }

        let footer = secondary(&format!("{status}  ·  Powered by GIPHY"), mtm);
        footer.setFont(Some(&NSFont::systemFontOfSize(10.5)));
        column.addArrangedSubview(&footer);
        self.present_popover(&column, &anchor, actions);
    }

    /// Clic sur un aperçu (son `tag` désigne l'action dans la bulle).
    pub(super) fn gif_tile_clicked(&self, recognizer: &NSClickGestureRecognizer) {
        let Some(view) = recognizer.view() else { return };
        self.popover_action(view.tag());
    }

    /// GIF choisi : ⌥-clic → favori ; sinon téléchargé et envoyé (en arrière-plan).
    fn gif_chosen(&self, gif: &GifResult) {
        let option = NSApplication::sharedApplication(self.mtm())
            .currentEvent()
            .is_some_and(|event| event.modifierFlags().contains(objc2_app_kit::NSEventModifierFlags::Option));
        if option {
            let favorite = self.engine().toggle_gif_favorite(gif);
            self.engine().set_status(if favorite { "GIF ajouté aux favoris" } else { "GIF retiré des favoris" });
            if self.ivars().gif.borrow().favorites_tab {
                self.gif_search();
            }
            return;
        }
        let Some(conversation) = self.current_conversation_id() else { return };
        self.close_popover();
        let engine = self.engine().clone();
        let gif = gif.clone();
        engine.set_status("Envoi du GIF…");
        std::thread::spawn(move || {
            if let Err(error) = engine.send_giphy(&conversation, &gif) {
                engine.set_status(format!("GIF non envoyé : {error}"));
            }
        });
    }

    /// Sans clé d'API : explication, lien vers GIPHY et champ pour la coller.
    fn gif_key_popover(&self) {
        let mtm = self.mtm();
        let Some(anchor) = self.ivars().gif.borrow().anchor.clone() else { return };
        let mut actions: Vec<Action> = Vec::new();
        let column = stack(true, 8.0, mtm);
        column.setEdgeInsets(NSEdgeInsets { top: 12.0, left: 10.0, bottom: 12.0, right: 10.0 });
        column.addArrangedSubview(&strong("Recherche de GIF avec GIPHY", 13.0, mtm));
        let text = paragraph(
            "Il faut une clé d'API GIPHY (gratuite) : créez-la sur developers.giphy.com, puis collez-la ici. \
             C'est le seul usage d'Internet avec les mises à jour.",
            mtm,
        );
        full_width(&column, &text);
        let tag = actions.len() as isize;
        actions.push(Box::new(|_c: &Controller| crate::shared::open_path("https://developers.giphy.com/dashboard/")));
        let create = button("Créer une clé sur GIPHY…", sel!(popoverAction:), self, mtm);
        create.setTag(tag);
        column.addArrangedSubview(&create);
        let row = stack(false, 6.0, mtm);
        let field = NSTextField::textFieldWithString(&ns(""), mtm);
        field.setPlaceholderString(Some(&ns("Clé d'API GIPHY")));
        field.setTranslatesAutoresizingMaskIntoConstraints(false);
        field.widthAnchor().constraintEqualToConstant(150.0).setActive(true);
        row.addArrangedSubview(&field);
        let tag = actions.len() as isize;
        let typed = field.clone();
        actions.push(Box::new(move |c: &Controller| {
            let key = typed.stringValue().to_string();
            match c.engine().set_giphy_key(&key) {
                Ok(()) if c.engine().giphy_key().is_some() => {
                    c.engine().set_status("Clé GIPHY enregistrée");
                    c.gif_search();
                }
                Ok(()) => {}
                Err(error) => c.engine().set_status(error.to_string()),
            }
        }));
        let save = button("Enregistrer", sel!(popoverAction:), self, mtm);
        save.setTag(tag);
        row.addArrangedSubview(&save);
        column.addArrangedSubview(&row);
        self.present_popover(&column, &anchor, actions);
    }
}
