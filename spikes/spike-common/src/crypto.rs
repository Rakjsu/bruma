//! Cripto do spike: identidade Ed25519, prekey X25519 derivada, e chave de sessão por ECDH.
//!
//! NOTA: aqui a chave de sessão vem de um ECDH estático-estático, portanto NÃO tem forward
//! secrecy. É deliberado — o spike só quer provar transporte + opacidade. No produto isto é
//! substituído pelas chaves de época por trás do trait `GroupKeyAgreement`.

use anyhow::{anyhow, Result};
use chacha20poly1305::{aead::Aead, KeyInit, XChaCha20Poly1305, XNonce};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use hkdf::Hkdf;
use sha2::Sha256;
use x25519_dalek::{PublicKey as XPublic, StaticSecret};

const CTX_X25519: &[u8] = b"bruma/spike1/x25519/v1";
const CTX_SESSION: &[u8] = b"bruma/spike1/session/v1";
const CTX_BIND: &[u8] = b"bruma/spike1/prekey-binding/v1";

pub struct Identity {
    pub signing: SigningKey,
    pub x_secret: StaticSecret,
}

impl Identity {
    /// Deriva tudo de uma única semente de 32 bytes — é ela que as 12 palavras vão recuperar.
    pub fn from_seed(seed: &[u8; 32]) -> Self {
        let signing = SigningKey::from_bytes(seed);
        let mut xb = [0u8; 32];
        Hkdf::<Sha256>::new(None, seed)
            .expand(CTX_X25519, &mut xb)
            .expect("hkdf x25519");
        Identity {
            signing,
            x_secret: StaticSecret::from(xb),
        }
    }

    pub fn verifying(&self) -> VerifyingKey {
        self.signing.verifying_key()
    }

    pub fn x_public(&self) -> XPublic {
        XPublic::from(&self.x_secret)
    }

    /// Assina a prekey para a prender à identidade. Sem isto, qualquer um anunciava a prekey de outro.
    pub fn prekey_signature(&self) -> [u8; 64] {
        self.signing
            .sign(&bind_msg(self.x_public().as_bytes()))
            .to_bytes()
    }
}

fn bind_msg(xpub: &[u8; 32]) -> Vec<u8> {
    let mut m = Vec::with_capacity(CTX_BIND.len() + 32);
    m.extend_from_slice(CTX_BIND);
    m.extend_from_slice(xpub);
    m
}

pub fn verify_prekey(id: &VerifyingKey, xpub: &[u8; 32], sig: &[u8; 64]) -> Result<()> {
    id.verify(&bind_msg(xpub), &Signature::from_bytes(sig))
        .map_err(|_| anyhow!("assinatura da prekey não corresponde à identidade do peer"))
}

/// ECDH + HKDF. O sal ordena as duas identidades para os dois lados derivarem a MESMA chave.
pub fn session_key(mine: &StaticSecret, theirs: &XPublic, a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    let shared = mine.diffie_hellman(theirs);
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    let mut salt = Vec::with_capacity(64);
    salt.extend_from_slice(lo);
    salt.extend_from_slice(hi);
    let mut key = [0u8; 32];
    Hkdf::<Sha256>::new(Some(&salt), shared.as_bytes())
        .expand(CTX_SESSION, &mut key)
        .expect("hkdf sessão");
    key
}

pub fn seal(key: &[u8; 32], plaintext: &[u8]) -> Result<([u8; 24], Vec<u8>)> {
    let cipher = XChaCha20Poly1305::new(key.into());
    let mut nonce = [0u8; 24];
    getrandom::getrandom(&mut nonce).map_err(|e| anyhow!("rng: {e}"))?;
    let ct = cipher
        .encrypt(XNonce::from_slice(&nonce), plaintext)
        .map_err(|_| anyhow!("falha a cifrar"))?;
    Ok((nonce, ct))
}

pub fn open(key: &[u8; 32], nonce: &[u8; 24], ct: &[u8]) -> Result<Vec<u8>> {
    XChaCha20Poly1305::new(key.into())
        .decrypt(XNonce::from_slice(nonce), ct)
        .map_err(|_| anyhow!("falha a decifrar"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEED_A: [u8; 32] = [1u8; 32];
    const SEED_B: [u8; 32] = [2u8; 32];

    #[test]
    fn identidade_e_deterministica() {
        // E isto que faz as 12 palavras funcionarem: mesma semente, mesma identidade.
        let a = Identity::from_seed(&SEED_A);
        let b = Identity::from_seed(&SEED_A);
        assert_eq!(a.verifying().as_bytes(), b.verifying().as_bytes());
        assert_eq!(a.x_public().as_bytes(), b.x_public().as_bytes());
    }

    #[test]
    fn prekey_valida_contra_a_propria_identidade() {
        let a = Identity::from_seed(&SEED_A);
        verify_prekey(
            &a.verifying(),
            a.x_public().as_bytes(),
            &a.prekey_signature(),
        )
        .expect("a prekey devia validar contra a propria identidade");
    }

    #[test]
    fn prekey_de_outro_e_rejeitada() {
        // Sem esta verificacao, qualquer um anunciava a prekey de outra pessoa e lia-lhe as mensagens.
        let a = Identity::from_seed(&SEED_A);
        let b = Identity::from_seed(&SEED_B);
        assert!(
            verify_prekey(
                &b.verifying(),
                a.x_public().as_bytes(),
                &a.prekey_signature()
            )
            .is_err(),
            "assinatura de A nao pode validar sob a identidade de B"
        );
    }

    #[test]
    fn os_dois_lados_derivam_a_mesma_chave() {
        let a = Identity::from_seed(&SEED_A);
        let b = Identity::from_seed(&SEED_B);
        let ka = session_key(
            &a.x_secret,
            &b.x_public(),
            a.verifying().as_bytes(),
            b.verifying().as_bytes(),
        );
        // B chama com os argumentos pela ordem dele. Tem de dar o mesmo.
        let kb = session_key(
            &b.x_secret,
            &a.x_public(),
            b.verifying().as_bytes(),
            a.verifying().as_bytes(),
        );
        assert_eq!(ka, kb, "o sal ordenado devia tornar a derivacao simetrica");
    }

    #[test]
    fn cifrar_e_decifrar_ida_e_volta() {
        let key = [7u8; 32];
        let (nonce, ct) = seal(&key, b"ola bruma").unwrap();
        assert_ne!(
            &ct[..],
            b"ola bruma",
            "o ciphertext nao pode conter o texto"
        );
        assert_eq!(open(&key, &nonce, &ct).unwrap(), b"ola bruma");
    }

    #[test]
    fn chave_errada_nao_decifra() {
        let (nonce, ct) = seal(&[7u8; 32], b"segredo").unwrap();
        assert!(open(&[8u8; 32], &nonce, &ct).is_err());
    }

    #[test]
    fn ciphertext_adulterado_e_rejeitado() {
        // AEAD: mexer num byte tem de falhar, nao devolver lixo.
        let key = [7u8; 32];
        let (nonce, mut ct) = seal(&key, b"segredo").unwrap();
        ct[0] ^= 0x01;
        assert!(open(&key, &nonce, &ct).is_err());
    }

    #[test]
    fn nonces_nao_se_repetem() {
        let key = [7u8; 32];
        let (n1, _) = seal(&key, b"a").unwrap();
        let (n2, _) = seal(&key, b"a").unwrap();
        assert_ne!(
            n1, n2,
            "reutilizar nonce com XChaCha20 quebra a confidencialidade"
        );
    }
}
