//! Cripto do spike: identidade Ed25519, prekey X25519 derivada, e chave de sessão por ECDH.
//!
//! NOTA: aqui a chave de sessão vem de um ECDH estático-estático, portanto NÃO tem forward
//! secrecy. É deliberado — o spike só quer provar transporte + opacidade. No produto isto é
//! substituído pelas chaves de época por trás do trait `GroupKeyAgreement`.

use anyhow::{anyhow, bail, Result};
use chacha20poly1305::{aead::Aead, KeyInit, XChaCha20Poly1305, XNonce};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use hkdf::Hkdf;
use sha2::Sha256;
use x25519_dalek::{PublicKey as XPublic, StaticSecret};

const CTX_CONVERSA: &[u8] = b"bruma/conversa/v1";
const CTX_X25519: &[u8] = b"bruma/spike1/x25519/v1";
const CTX_SESSION: &[u8] = b"bruma/spike1/session/v1";
const CTX_BIND: &[u8] = b"bruma/spike1/prekey-binding/v1";
/// Contexto da chave que cifra o índice local. Separado dos outros de propósito: uma chave
/// por finalidade significa que comprometer uma não entrega as outras.
const CTX_INDICE: &[u8] = b"bruma/indice/v1";

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

/// O identificador da conversa entre duas identidades.
///
/// Determinístico e simétrico: as duas máquinas chegam ao mesmo id **sem trocarem uma
/// palavra sobre isso**. É o que dispensa qualquer convite — e, ao contrário do convite de
/// servidor, não há aqui segredo nenhum para transportar, portanto não há nada que se possa
/// reencaminhar a um terceiro.
///
/// Ordenam-se as duas chaves pelo mesmo motivo que em `session_key`: quem abre a conversa
/// primeiro não pode mudar o resultado.
/// Quatro bytes de soma de controlo sobre uns bytes quaisquer.
///
/// Serve para distinguir «esta é a minha semente» de «esta é a minha semente com um bit
/// virado». Sem isto, um bit trocado no ficheiro da identidade fazia a app arrancar como
/// OUTRA pessoa, em silêncio e para sempre. Quatro bytes é 1 em 4 mil milhões — de sobra para
/// apanhar corrupção acidental, que é o que isto defende (não é uma assinatura).
pub fn soma_de_controlo(bytes: &[u8]) -> [u8; 4] {
    let mut h = blake3::Hasher::new();
    h.update(b"bruma/soma/v1");
    h.update(bytes);
    let mut c = [0u8; 4];
    c.copy_from_slice(&h.finalize().as_bytes()[..4]);
    c
}

pub fn id_da_conversa(a: &[u8; 32], b: &[u8; 32]) -> [u8; 16] {
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    let mut h = blake3::Hasher::new();
    h.update(CTX_CONVERSA);
    h.update(lo);
    h.update(hi);
    let mut id = [0u8; 16];
    id.copy_from_slice(&h.finalize().as_bytes()[..16]);
    id
}

/// ECDH + HKDF. O sal ordena as duas identidades para os dois lados derivarem a MESMA chave.
/// # Porque é que isto devolve `Result`
///
/// Uma prekey assinada prova que aquela pessoa a **anunciou** — não prova que ela conhece o
/// segredo correspondente, nem que os 32 bytes são sequer um ponto útil. Existem pontos de
/// ordem pequena (o zero é o mais simples) para os quais o Diffie-Hellman devolve sempre
/// zeros, aconteça o que acontecer do outro lado.
///
/// Anunciar um desses transforma a chave da conversa numa função **só de dados públicos**:
/// `HKDF(sal = as duas identidades, ikm = 0)`. Qualquer pessoa que veja as duas chaves
/// públicas a calcula, e lê a conversa toda. E não haveria sintoma nenhum — a conversa
/// funciona, cifra, decifra, e está aberta a quem passar.
///
/// O `was_contributory` é a pergunta «o meu segredo contou para alguma coisa?». Se não
/// contou, isto não é uma chave partilhada; é um número que ambos os lados sabiam de antemão.
pub fn session_key(
    mine: &StaticSecret,
    theirs: &XPublic,
    a: &[u8; 32],
    b: &[u8; 32],
) -> Result<[u8; 32]> {
    let shared = mine.diffie_hellman(theirs);
    if !shared.was_contributory() {
        bail!("a chave de conversa anunciada não serve: o segredo partilhado seria público");
    }
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    let mut salt = Vec::with_capacity(64);
    salt.extend_from_slice(lo);
    salt.extend_from_slice(hi);
    let mut key = [0u8; 32];
    Hkdf::<Sha256>::new(Some(&salt), shared.as_bytes())
        .expand(CTX_SESSION, &mut key)
        .expect("hkdf sessão");
    Ok(key)
}

/// A semente escrita em palavras, para se poder guardar num papel.
///
/// # Porquê palavras e não um ficheiro
///
/// Um ficheiro copiado vive no mesmo disco que morre. As palavras cabem num papel dentro de
/// uma gaveta, e sobrevivem ao computador — é essa a diferença que interessa.
///
/// São VINTE E QUATRO e não doze porque a semente tem 32 bytes, e 32 bytes são 24 palavras
/// no BIP39. Doze palavras exigiriam uma semente de 16 bytes, e mudá-la agora invalidava
/// todas as identidades que já existem. Prometeram-se doze antes de o código existir; a
/// verdade são vinte e quatro, e é isso que se escreve.
///
/// **Isto é a identidade inteira.** Quem tiver estas palavras é a pessoa: lê o histórico,
/// entra nas salas, fala em nome dela. Guardam-se como se guarda uma chave de casa.
pub fn semente_em_palavras(seed: &[u8; 32]) -> Result<String> {
    let m = bip39::Mnemonic::from_entropy(seed).map_err(|e| anyhow!("não deu palavras: {e}"))?;
    Ok(m.to_string())
}

/// O caminho de volta: das palavras à semente.
///
/// Aceita as palavras como a pessoa as escrever — espaços a mais, linhas partidas,
/// maiúsculas. Quem copia de um papel escrito à mão vai errar nisso, e recusar por causa de
/// um espaço seria transformar uma recuperação numa adivinha.
pub fn palavras_em_semente(texto: &str) -> Result<[u8; 32]> {
    let limpo = texto
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    // As mensagens do bip39 vêm em inglês e falam de "mnemonic" e "checksum". Quem está a
    // recuperar uma identidade está normalmente a ter um mau dia; a mensagem tem de dizer o
    // que fazer, e não o nome do algoritmo.
    let m = bip39::Mnemonic::parse_normalized(&limpo).map_err(|e| {
        use bip39::Error::*;
        match e {
            BadWordCount(n) => anyhow!(
                "contei {n} palavras e são precisas 24 — falta alguma, ou sobra?"
            ),
            UnknownWord(i) => {
                let qual = limpo.split(' ').nth(i).unwrap_or("?");
                anyhow!(
                    "a palavra {} (\"{qual}\") não é do dicionário — vê se está bem escrita",
                    i + 1
                )
            }
            InvalidChecksum => anyhow!(
                "as palavras estão todas certas mas a ordem ou uma delas não bate certo. Confere a lista do princípio ao fim"
            ),
            outro => anyhow!("essas palavras não servem: {outro}"),
        }
    })?;
    let (bytes, n) = m.to_entropy_array();
    if n != 32 {
        return Err(anyhow!(
            "essas palavras dão {n} bytes e a identidade precisa de 32 — faltam palavras?"
        ));
    }
    let mut s = [0u8; 32];
    s.copy_from_slice(&bytes[..32]);
    Ok(s)
}

/// A chave que cifra o índice local, derivada da mesma semente da identidade.
///
/// # Porque é que isto tem de existir
///
/// O `indice.json` guarda a chave simétrica de **cada servidor**, ao lado do histórico que
/// essas chaves decifram. Enquanto esteve em texto simples, a cifra do histórico não
/// protegia nada de quem tivesse acesso à pasta: o cofre estava fechado e a chave colada
/// por fora.
///
/// Não pede password nenhuma, e é deliberado: o Bruma não tem passwords, e inventar uma só
/// para isto mudava a promessa do produto. Isto protege contra quem lê a pasta — uma cópia
/// de segurança na nuvem, um disco emprestado, um backup que sai de casa — e não contra
/// quem já tem a `identidade.key`. Quem tem a identidade é, para todos os efeitos, a pessoa.
pub fn chave_do_indice(seed: &[u8; 32]) -> [u8; 32] {
    let mut k = [0u8; 32];
    Hkdf::<Sha256>::new(None, seed)
        .expand(CTX_INDICE, &mut k)
        .expect("hkdf índice");
    k
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
mod testes_palavras {
    use super::*;

    /// Uma prekey de ordem pequena tornava a chave da conversa calculavel por QUALQUER UM.
    ///
    /// Uma assinatura sobre a prekey prova que ela foi anunciada por aquela pessoa. Nao prova
    /// que ela conhece o segredo, nem que os 32 bytes servem para alguma coisa. Anunciando um
    /// ponto degenerado, o Diffie-Hellman devolve zeros aconteca o que acontecer do outro
    /// lado, e a chave passa a ser uma funcao so de dados publicos -- sem sintoma nenhum: a
    /// conversa cifra, decifra, e esta aberta a quem passar.
    #[test]
    fn uma_prekey_degenerada_e_recusada() {
        let eu = Identity::from_seed(&[1u8; 32]);
        let ed = eu.verifying().to_bytes();
        let outro = [9u8; 32];

        let zeros = XPublic::from([0u8; 32]);
        assert!(
            session_key(&eu.x_secret, &zeros, &ed, &outro).is_err(),
            "aceitou a prekey de zeros -- a chave seria publica"
        );
        let mut um = [0u8; 32];
        um[0] = 1;
        assert!(
            session_key(&eu.x_secret, &XPublic::from(um), &ed, &outro).is_err(),
            "aceitou um ponto de ordem pequena"
        );

        // E uma prekey de verdade continua a passar -- uma recusa que recusasse tudo tambem
        // nao servia de nada.
        let dele = Identity::from_seed(&[2u8; 32]);
        assert!(
            session_key(
                &eu.x_secret,
                &dele.x_public(),
                &ed,
                &dele.verifying().to_bytes()
            )
            .is_ok(),
            "recusou uma prekey boa"
        );
    }

    /// Os dois lados TEM de chegar ao mesmo id, e a ordem nao pode contar.
    ///
    /// Se contasse, cada um abria a sua conversa, escrevia no seu log, e nenhum via o do
    /// outro -- dois monologos em vez de uma conversa, sem um unico erro pelo caminho.
    #[test]
    fn o_id_da_conversa_e_o_mesmo_dos_dois_lados() {
        let a = [7u8; 32];
        let b = [200u8; 32];
        assert_eq!(
            id_da_conversa(&a, &b),
            id_da_conversa(&b, &a),
            "a ordem contou"
        );

        // E pares diferentes tem de dar ids diferentes -- senao duas conversas partilhavam
        // o mesmo log.
        let c = [9u8; 32];
        assert_ne!(id_da_conversa(&a, &b), id_da_conversa(&a, &c));
        assert_ne!(id_da_conversa(&a, &b), id_da_conversa(&b, &c));
    }

    /// A chave da conversa tambem: mesma dos dois lados, e diferente para cada par.
    #[test]
    fn a_chave_da_conversa_e_a_mesma_dos_dois_lados() {
        let ia = Identity::from_seed(&[1u8; 32]);
        let ib = Identity::from_seed(&[2u8; 32]);
        let ea = ia.verifying().to_bytes();
        let eb = ib.verifying().to_bytes();
        let ka = session_key(&ia.x_secret, &ib.x_public(), &ea, &eb).expect("chave de sessão");
        let kb = session_key(&ib.x_secret, &ia.x_public(), &eb, &ea).expect("chave de sessão");
        assert_eq!(ka, kb, "os dois lados derivaram chaves diferentes");

        let ic = Identity::from_seed(&[3u8; 32]);
        let ec = ic.verifying().to_bytes();
        let kc = session_key(&ia.x_secret, &ic.x_public(), &ea, &ec).expect("chave de sessão");
        assert_ne!(ka, kc, "duas conversas diferentes com a mesma chave");
    }

    #[test]
    fn as_palavras_devolvem_a_mesma_semente() {
        let semente = [7u8; 32];
        let texto = semente_em_palavras(&semente).unwrap();
        assert_eq!(
            texto.split_whitespace().count(),
            24,
            "32 bytes dao 24 palavras"
        );
        assert_eq!(palavras_em_semente(&texto).unwrap(), semente);
    }

    /// Quem copia de um papel escrito a mao vai errar nos espacos e nas maiusculas.
    /// Recusar por causa disso seria transformar uma recuperacao numa adivinha.
    #[test]
    fn aceita_o_que_uma_pessoa_escreveria() {
        let semente = [200u8; 32];
        let texto = semente_em_palavras(&semente).unwrap();
        let sujo = format!(
            "  {}  ",
            texto.to_uppercase().replace(
                ' ', "
  "
            )
        );
        assert_eq!(palavras_em_semente(&sujo).unwrap(), semente);
    }

    /// Uma palavra trocada tem de FALHAR. O BIP39 tem soma de controlo precisamente para
    /// isto: sem ela, um erro de copia dava uma identidade diferente em silencio.
    /// Uma palavra que nao existe no dicionario tem de ser apontada PELO NUMERO e pelo
    /// texto. "zebra" nao serve para este teste: esta no dicionario do BIP39, e o erro
    /// passa a ser de soma de controlo -- foi o proprio teste que mo ensinou.
    #[test]
    fn uma_palavra_inventada_e_apontada() {
        let texto = semente_em_palavras(&[3u8; 32]).unwrap();
        let mut ps: Vec<&str> = texto.split(' ').collect();
        ps[5] = "xyzzy";
        let e = palavras_em_semente(&ps.join(" ")).unwrap_err().to_string();
        assert!(
            e.contains("xyzzy") && e.contains('6'),
            "tem de dizer QUAL: {e}"
        );
    }

    /// A ordem trocada passa pelo dicionario e falha na soma de controlo. A mensagem tem de
    /// mandar conferir a lista, e nao falar de "checksum".
    #[test]
    fn a_ordem_trocada_diz_o_que_fazer() {
        let texto = semente_em_palavras(&[11u8; 32]).unwrap();
        let mut ps: Vec<&str> = texto.split(' ').collect();
        ps.swap(0, 1);
        let e = palavras_em_semente(&ps.join(" ")).unwrap_err().to_string();
        assert!(e.contains("ordem") || e.contains("Confere"), "{e}");
        assert!(!e.to_lowercase().contains("checksum"), "sem jargao: {e}");
    }

    /// Poucas palavras tem de dizer QUANTAS contou, para a pessoa saber onde procurar.
    /// Poucas palavras tem de dizer QUANTAS contou. As palavras usadas tem de ser do
    /// dicionario, senao o erro que salta primeiro e o da palavra desconhecida.
    #[test]
    fn conta_as_palavras_que_encontrou() {
        let texto = semente_em_palavras(&[5u8; 32]).unwrap();
        let poucas: Vec<&str> = texto.split(' ').take(3).collect();
        let e = palavras_em_semente(&poucas.join(" "))
            .unwrap_err()
            .to_string();
        assert!(e.contains('3') && e.contains("24"), "{e}");
    }

    /// Doze palavras dao 16 bytes, e a identidade precisa de 32. Tem de dizer isso, e nao
    /// aceitar e criar meia identidade.
    #[test]
    fn doze_palavras_nao_chegam() {
        let curto = bip39::Mnemonic::from_entropy(&[9u8; 16])
            .unwrap()
            .to_string();
        assert_eq!(curto.split_whitespace().count(), 12);
        let e = palavras_em_semente(&curto).unwrap_err().to_string();
        assert!(e.contains("32"), "a mensagem tem de explicar: {e}");
    }

    /// A identidade derivada das palavras tem de ser a MESMA — e nao so a semente igual.
    #[test]
    fn a_identidade_sobrevive_a_viagem() {
        let semente = [42u8; 32];
        let antes = Identity::from_seed(&semente);
        let texto = semente_em_palavras(&semente).unwrap();
        let depois = Identity::from_seed(&palavras_em_semente(&texto).unwrap());
        assert_eq!(antes.verifying().as_bytes(), depois.verifying().as_bytes());
        assert_eq!(antes.x_public().as_bytes(), depois.x_public().as_bytes());
    }

    /// A chave do indice tem de ser DIFERENTE da de sessao, com a mesma semente. Uma chave
    /// por finalidade: comprometer uma nao entrega as outras.
    #[test]
    fn a_chave_do_indice_e_so_dela() {
        let semente = [1u8; 32];
        let ki = chave_do_indice(&semente);
        assert_ne!(ki, semente, "a chave nao pode ser a propria semente");
        assert_ne!(
            ki,
            chave_do_indice(&[2u8; 32]),
            "sementes diferentes, chaves diferentes"
        );
        // e o que ela cifra, so ela decifra
        let (n, ct) = seal(&ki, b"as chaves dos servidores").unwrap();
        assert!(open(&chave_do_indice(&[2u8; 32]), &n, &ct).is_err());
        assert_eq!(open(&ki, &n, &ct).unwrap(), b"as chaves dos servidores");
    }
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
        )
        .expect("chave de sessão");
        // B chama com os argumentos pela ordem dele. Tem de dar o mesmo.
        let kb = session_key(
            &b.x_secret,
            &a.x_public(),
            b.verifying().as_bytes(),
            a.verifying().as_bytes(),
        )
        .expect("chave de sessão");
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
