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
    /// Ferme après avoir envoyé la dernière réponse, sans la perdre (voir TCP).
    fn close_after_reply(&self) {
        self.close();
    }
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

    /// Fermer un socket qui contient encore des données non lues envoie un RST, et Windows jette
    /// alors chez l'expéditeur ce qu'il n'avait pas encore lu (notre refus, par exemple). On cesse
    /// donc d'écrire et on lit ce qui arrive encore jusqu'à ce que l'autre ferme (2 s et 1 Mo au
    /// plus).
    fn close_after_reply(&self) {
        let _ = self.shutdown(Shutdown::Write);
        TcpStream::set_read_timeout(self, Some(Duration::from_secs(2))).ok();
        let mut reader = self;
        let mut buffer = [0_u8; 8192];
        let mut drained = 0;
        while drained < 1024 * 1024 {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(size) => drained += size,
            }
        }
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

    fn close_after_reply(&self) {
        (**self).close_after_reply()
    }
}
