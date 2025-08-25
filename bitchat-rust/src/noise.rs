//! Manages Noise Protocol sessions for secure communication.

use snow::{Builder, HandshakeState, TransportState};
use std::mem;

const NOISE_PARAMS: &str = "Noise_XX_25519_ChaChaPoly_SHA256";

pub struct NoiseSession {
    state: NoiseState,
}

enum NoiseState {
    Handshake(HandshakeState),
    Transport(TransportState),
    Transitioning, // Temporary state for ownership transfer
}

impl NoiseSession {
    /// Create a new Noise session as the initiator.
    pub fn new_initiator() -> Result<(Self, Vec<u8>), snow::Error> {
        let builder = Builder::new(NOISE_PARAMS.parse()?);
        let keypair = builder.generate_keypair()?;
        let mut handshake = builder.local_private_key(&keypair.private).build_initiator()?;

        // Perform the first step of the XX handshake (-> e)
        let mut first_msg = vec![0u8; 200];
        let len = handshake.write_message(&[], &mut first_msg)?;
        first_msg.truncate(len);

        Ok((
            Self {
                state: NoiseState::Handshake(handshake),
            },
            first_msg,
        ))
    }

    /// Create a new Noise session as the responder.
    pub fn new_responder() -> Result<Self, snow::Error> {
        let builder = Builder::new(NOISE_PARAMS.parse()?);
        let keypair = builder.generate_keypair()?;
        let handshake = builder.local_private_key(&keypair.private).build_responder()?;
        Ok(Self {
            state: NoiseState::Handshake(handshake),
        })
    }

    /// Process a handshake message and potentially transition to transport mode.
    /// Returns a message to be sent to the other party, if any.
    pub fn handshake_read(&mut self, input: &[u8]) -> Result<Option<Vec<u8>>, snow::Error> {
        let current_state = mem::replace(&mut self.state, NoiseState::Transitioning);
        let mut handshake = match current_state {
            NoiseState::Handshake(h) => h,
            NoiseState::Transport(t) => {
                self.state = NoiseState::Transport(t); // Put it back
                return Ok(None); // Already in transport mode
            },
            NoiseState::Transitioning => return Err(snow::Error::Input), // Was State
        };

        let mut reply = vec![0u8; 200];
        let mut read_buf = vec![0u8; 200];
        handshake.read_message(input, &mut read_buf)?;

        let reply_opt = if !handshake.is_handshake_finished() {
            let len = handshake.write_message(&[], &mut reply)?;
            reply.truncate(len);
            Some(reply)
        } else {
            None
        };

        if handshake.is_handshake_finished() {
            self.state = NoiseState::Transport(handshake.into_transport_mode()?);
        } else {
            self.state = NoiseState::Handshake(handshake);
        }

        Ok(reply_opt)
    }

    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, snow::Error> {
        let transport = match &mut self.state {
            NoiseState::Transport(t) => t,
            _ => return Err(snow::Error::Input), // Was State
        };
        let mut ciphertext = vec![0u8; plaintext.len() + 100];
        let len = transport.write_message(plaintext, &mut ciphertext)?;
        ciphertext.truncate(len);
        Ok(ciphertext)
    }

    pub fn decrypt(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>, snow::Error> {
        let transport = match &mut self.state {
            NoiseState::Transport(t) => t,
            _ => return Err(snow::Error::Input), // Was State
        };
        let mut plaintext = vec![0u8; ciphertext.len()];
        let len = transport.read_message(ciphertext, &mut plaintext)?;
        plaintext.truncate(len);
        Ok(plaintext)
    }

    pub fn is_transport_mode(&self) -> bool {
        matches!(self.state, NoiseState::Transport(_))
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_full_handshake_and_transport() {
        // 1. Initiator starts
        let (mut initiator, init_msg) = NoiseSession::new_initiator().unwrap();

        // 2. Responder receives the first message
        let mut responder = NoiseSession::new_responder().unwrap();
        let resp_msg = responder.handshake_read(&init_msg).unwrap().unwrap();

        // 3. Initiator receives the response
        let final_msg = initiator.handshake_read(&resp_msg).unwrap().unwrap();

        // 4. Responder receives the final message
        let no_reply = responder.handshake_read(&final_msg).unwrap();
        assert!(no_reply.is_none());

        // 5. Both should now be in transport mode
        assert!(matches!(initiator.state, NoiseState::Transport(_)));
        assert!(matches!(responder.state, NoiseState::Transport(_)));

        // 6. Test transport messages
        let plaintext1 = b"hello from initiator";
        let ciphertext1 = initiator.encrypt(plaintext1).unwrap();
        let decrypted1 = responder.decrypt(&ciphertext1).unwrap();
        assert_eq!(plaintext1, decrypted1.as_slice());

        let plaintext2 = b"hello from responder";
        let ciphertext2 = responder.encrypt(plaintext2).unwrap();
        let decrypted2 = initiator.decrypt(&ciphertext2).unwrap();
        assert_eq!(plaintext2, decrypted2.as_slice());
    }
}
