//! File dialogs, shown from a background thread.
//!
//! On Linux the system's file chooser (through the XDG desktop portal) is
//! waited for on the thread that asks for it. Asked from the UI thread,
//! the window stops drawing and answering the desktop for as long as the
//! chooser is open, and after a few seconds GNOME offers to force quit
//! k-pdf. So dialogs run on a thread of their own, and the answer is picked
//! up with [`Dialog::poll`] together with what it was asked for. (Where the
//! system needs dialogs on the main thread, as on macOS, rfd hands them
//! over itself.)

use std::path::PathBuf;
use std::sync::mpsc;

/// The dialog that is open, if one is: what it was asked for, and where its
/// answer comes.
pub struct Dialog<T> {
    open: Option<(T, mpsc::Receiver<Option<Vec<PathBuf>>>)>,
}

impl<T> Default for Dialog<T> {
    fn default() -> Self {
        Dialog { open: None }
    }
}

impl<T> Dialog<T> {
    /// Asks for files to open, for `purpose`. (Scripted runs, which can't
    /// answer the dialog, take them from `K_PDF_SCRIPT_PICK`, separated by
    /// '|'.)
    pub fn pick(&mut self, ctx: &eframe::egui::Context, purpose: T, dialog: rfd::FileDialog) {
        #[cfg(feature = "script")]
        if let Some(paths) = std::env::var_os("K_PDF_SCRIPT_PICK") {
            let paths = paths.to_string_lossy().split('|').map(PathBuf::from).collect();
            return self.answer_now(purpose, Some(paths));
        }
        self.show(ctx, purpose, move || dialog.pick_files());
    }

    /// Asks where to save, for `purpose`.
    pub fn save(&mut self, ctx: &eframe::egui::Context, purpose: T, dialog: rfd::FileDialog) {
        self.show(ctx, purpose, move || dialog.save_file().map(|p| vec![p]));
    }

    #[cfg(feature = "script")]
    fn answer_now(&mut self, purpose: T, answer: Option<Vec<PathBuf>>) {
        if self.open.is_none() {
            let (tx, rx) = mpsc::channel();
            tx.send(answer).ok();
            self.open = Some((purpose, rx));
        }
    }

    /// Shows a dialog on its own thread; while one is open, others asked
    /// for are ignored.
    fn show(&mut self, ctx: &eframe::egui::Context, purpose: T, run: impl FnOnce() -> Option<Vec<PathBuf>> + Send + 'static) {
        if self.open.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            tx.send(run()).ok();
            ctx.request_repaint();
        });
        self.open = Some((purpose, rx));
    }

    /// The answer, once the dialog is closed: what it was asked for, and
    /// the files chosen (`None` when cancelled).
    pub fn poll(&mut self) -> Option<(T, Option<Vec<PathBuf>>)> {
        let (_, rx) = self.open.as_ref()?;
        let answer = match rx.try_recv() {
            Ok(answer) => answer.filter(|paths| !paths.is_empty()),
            Err(mpsc::TryRecvError::Empty) => return None,
            // The dialog's thread ended without answering (it panicked).
            Err(mpsc::TryRecvError::Disconnected) => None,
        };
        self.open.take().map(|(purpose, _)| (purpose, answer))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_caller_keeps_going_while_a_dialog_is_open() {
        let ctx = eframe::egui::Context::default();
        let mut dialog: Dialog<&str> = Dialog::default();
        // A "dialog" that stays open until told to close.
        let (close, closed) = mpsc::channel::<()>();
        dialog.show(&ctx, "open", move || {
            closed.recv().ok();
            Some(vec![PathBuf::from("a.jpg")])
        });
        // Still open: nothing yet, and another dialog isn't shown.
        assert!(dialog.poll().is_none());
        dialog.show(&ctx, "other", || panic!("shown while another is open"));
        close.send(()).unwrap();
        let answer = loop {
            if let Some(answer) = dialog.poll() {
                break answer;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        };
        assert_eq!(answer, ("open", Some(vec![PathBuf::from("a.jpg")])));
        assert!(dialog.poll().is_none());
    }
}
