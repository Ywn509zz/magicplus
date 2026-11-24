//! Lien d'octets sous le canal chiffré : TCP (réseau local) ou Bluetooth (RFCOMM).
//!
//! Le canal Noise (`secure.rs`) ne demande qu'un flux qui lit et écrit ; ce trait ajoute ce que
//! le protocole utilise en plus : délai de lecture, fermeture.

use std::{
    io::{self, Read, Write},
    net::{Shutdown, TcpStream},
    time::Duration,
};

pub(crate) trait Link: Read + Write + Send {
    /// Ferme le sens « envoi » seulement, si le lien le sait (TCP). Sinon `false` : le canal
    /// chiffré envoie alors une marque de fin (Bluetooth n'a pas de demi-fermeture).
    fn half_close(&self) -> bool;
    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()>;
    fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()>;
    /// Ferme tout.
    fn close(&self);
}

impl Link for TcpStream {
    fn half_close(&self) -> bool {
        self.shutdown(Shutdown::Write).is_ok()
    }

    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        TcpStream::set_read_timeout(self, timeout)
    }

    fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        TcpStream::set_write_timeout(self, timeout)
    }

    fn close(&self) {
        let _ = self.shutdown(Shutdown::Both);
    }
}

/// Lien d'un type quelconque.
pub(crate) type BoxLink = Box<dyn Link>;

impl<L: Link + ?Sized> Link for Box<L> {
    fn half_close(&self) -> bool {
        (**self).half_close()
    }

    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        (**self).set_read_timeout(timeout)
    }

    fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        (**self).set_write_timeout(timeout)
    }

    fn close(&self) {
        (**self).close()
    }
}
