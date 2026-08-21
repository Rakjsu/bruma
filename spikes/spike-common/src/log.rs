//! Log append-only, assinado e encadeado por hash — o desenho que substituiu o CRDT nas mensagens.
//!
//! A cadeia não é decorativa: cada entrada aponta para a cabeça que o autor via, esse ponteiro
//! está coberto pela assinatura, e é dele que sai a ORDEM das mensagens (ver `instantes`) e a
//! deteção de buracos no histórico (ver `orfas`).
//!
//! Guardado como JSON com campos em hex de propósito: dá para fazer `cat` ao ficheiro e VER
//! que o conteúdo é opaco. Isso é metade da verificação do spike.

use anyhow::{anyhow, bail, Result};
use data_encoding::HEXLOWER;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub const ZERO_HASH: [u8; 32] = [0u8; 32];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub author: String, // hex, 32 bytes — é também o EndpointId do iroh
    pub ts_ms: u64,
    pub prev: String,  // hex, 32 bytes — a cabeça que o autor via; define a ordem
    pub nonce: String, // hex, 24 bytes
    pub ciphertext: String, // hex
    pub sig: String,   // hex, 64 bytes
}

impl Entry {
    /// Bytes canónicos sobre os quais se calcula o hash. A ordem é parte do protocolo.
    fn canonical(&self) -> Result<Vec<u8>> {
        let mut b = Vec::new();
        b.extend_from_slice(&hex32(&self.author)?);
        b.extend_from_slice(&self.ts_ms.to_be_bytes());
        b.extend_from_slice(&hex32(&self.prev)?);
        b.extend_from_slice(&hexn::<24>(&self.nonce)?);
        b.extend_from_slice(&HEXLOWER.decode(self.ciphertext.as_bytes())?);
        Ok(b)
    }

    pub fn hash(&self) -> Result<[u8; 32]> {
        Ok(*blake3::hash(&self.canonical()?).as_bytes())
    }

    pub fn hash_hex(&self) -> Result<String> {
        Ok(HEXLOWER.encode(&self.hash()?))
    }

    pub fn verify(&self) -> Result<()> {
        let author = VerifyingKey::from_bytes(&hex32(&self.author)?)
            .map_err(|_| anyhow!("chave de autor inválida"))?;
        let sig = Signature::from_bytes(&hexn::<64>(&self.sig)?);
        author
            .verify(&self.hash()?, &sig)
            .map_err(|_| anyhow!("assinatura da entrada não confere"))
    }
}

pub struct Log {
    /// Indexado por hash em hex — a deduplicação sai de graça.
    entries: BTreeMap<String, Entry>,
    path: std::path::PathBuf,
}

impl Log {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let entries = if path.exists() {
            let raw = std::fs::read_to_string(&path)?;
            let list: Vec<Entry> = serde_json::from_str(&raw)?;
            let mut m = BTreeMap::new();
            for e in list {
                // Recusa entradas adulteradas mesmo vindas do disco.
                e.verify()?;
                m.insert(e.hash_hex()?, e);
            }
            m
        } else {
            BTreeMap::new()
        };
        Ok(Log { entries, path })
    }

    fn save(&self) -> Result<()> {
        let list = self.ordered();
        std::fs::write(&self.path, serde_json::to_string_pretty(&list)?)?;
        Ok(())
    }

    /// Instante efetivo de cada entrada, num relógio lógico híbrido.
    ///
    /// O relógio de parede sozinho não serve. Entre uma máquina nos EUA e outra no Brasil,
    /// alguns segundos de desvio bastam para uma resposta aparecer ANTES da pergunta — e é o
    /// tipo de bug que nunca se vê em testes locais, porque aí os relógios coincidem.
    ///
    /// A causalidade pura também não serve sozinha: agruparia mensagens concorrentes por uma
    /// ordem que não corresponde a nada que o utilizador reconheça.
    ///
    /// A regra é `instante(e) = max(e.ts_ms, instante(pai) + 1)`. Com os relógios em sintonia
    /// dá exatamente a ordem de parede; com um relógio atrasado, a entrada é empurrada para
    /// depois do pai em vez de saltar para trás. Depende só do grafo e dos carimbos guardados,
    /// portanto todos os peers convergem para a mesma ordem sem falarem uns com os outros.
    fn instantes(&self) -> BTreeMap<String, u64> {
        let mut cache: BTreeMap<String, u64> = BTreeMap::new();
        for inicio in self.entries.keys() {
            if cache.contains_key(inicio) {
                continue;
            }
            // Iterativo e não recursivo de propósito: uma conversa longa é uma cadeia longa,
            // e recursão aqui seria estouro de pilha à espera de acontecer.
            let mut pilha: Vec<String> = Vec::new();
            let mut atual = inicio.clone();
            loop {
                if cache.contains_key(&atual) {
                    break;
                }
                let Some(e) = self.entries.get(&atual) else {
                    break;
                };
                pilha.push(atual.clone());
                // Um ciclo exigiria colisão de hash (o `prev` está dentro do hash), mas o
                // limite custa nada e evita pendurar o processo se alguma vez existir.
                if !self.entries.contains_key(&e.prev) || pilha.len() > self.entries.len() {
                    break;
                }
                atual = e.prev.clone();
            }
            while let Some(h) = pilha.pop() {
                let e = &self.entries[&h];
                // Pai ausente (ainda não sincronizado): a entrada é uma raiz e vale o seu
                // próprio relógio. Quando o pai chegar, isto recalcula-se sozinho.
                let base = cache.get(&e.prev).copied().unwrap_or(0);
                let v = e.ts_ms.max(base.saturating_add(1));
                cache.insert(h, v);
            }
        }
        cache
    }

    /// Ordem determinística e igual em todos os peers: (instante efetivo, hash).
    pub fn ordered(&self) -> Vec<Entry> {
        let inst = self.instantes();
        let mut v: Vec<(u64, &String, &Entry)> = self
            .entries
            .iter()
            .map(|(h, e)| (inst.get(h).copied().unwrap_or(e.ts_ms), h, e))
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(b.1)));
        v.into_iter().map(|(_, _, e)| e.clone()).collect()
    }

    /// Entradas cujo pai ainda não chegou.
    ///
    /// É isto que torna o campo `prev` verificável em vez de decorativo: enquanto isto não
    /// for vazio, o histórico tem buracos e ainda não se pode chamar-lhe uma cadeia. Não é
    /// motivo para rejeitar nada — na sincronização as entradas chegam por qualquer ordem —
    /// mas é um sinal honesto de que falta material.
    pub fn orfas(&self) -> Vec<String> {
        let zero = HEXLOWER.encode(&ZERO_HASH);
        self.entries
            .iter()
            .filter(|(_, e)| e.prev != zero && !self.entries.contains_key(&e.prev))
            .map(|(h, _)| h.clone())
            .collect()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn head(&self) -> String {
        self.ordered()
            .last()
            .and_then(|e| e.hash_hex().ok())
            .unwrap_or_else(|| HEXLOWER.encode(&ZERO_HASH))
    }

    pub fn append_local(
        &mut self,
        signing: &SigningKey,
        nonce: [u8; 24],
        ciphertext: Vec<u8>,
        ts_ms: u64,
    ) -> Result<Entry> {
        let mut e = Entry {
            author: HEXLOWER.encode(signing.verifying_key().as_bytes()),
            ts_ms,
            prev: self.head(),
            nonce: HEXLOWER.encode(&nonce),
            ciphertext: HEXLOWER.encode(&ciphertext),
            sig: String::new(),
        };
        e.sig = HEXLOWER.encode(&signing.sign(&e.hash()?).to_bytes());
        self.entries.insert(e.hash_hex()?, e.clone());
        self.save()?;
        Ok(e)
    }

    /// Devolve quantas entradas eram novas. Entradas inválidas são rejeitadas, não confiadas.
    pub fn merge(&mut self, incoming: Vec<Entry>) -> Result<usize> {
        let mut added = 0;
        for e in incoming {
            if e.verify().is_err() {
                eprintln!("  [!] entrada rejeitada: assinatura inválida");
                continue;
            }
            // `entry` em vez de contains_key+insert: uma travessia da arvore em vez de duas.
            // Nota: caminho completo porque `Entry` aqui colidiria com o nosso struct Entry.
            if let std::collections::btree_map::Entry::Vacant(slot) =
                self.entries.entry(e.hash_hex()?)
            {
                slot.insert(e);
                added += 1;
            }
        }
        if added > 0 {
            self.save()?;
        }
        Ok(added)
    }
}

fn hex32(s: &str) -> Result<[u8; 32]> {
    hexn::<32>(s)
}

fn hexn<const N: usize>(s: &str) -> Result<[u8; N]> {
    let v = HEXLOWER.decode(s.as_bytes())?;
    if v.len() != N {
        bail!("esperava {N} bytes, recebi {}", v.len());
    }
    let mut out = [0u8; N];
    out.copy_from_slice(&v);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    fn tmp(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "bruma-spike1-test-{name}-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn key(b: u8) -> SigningKey {
        SigningKey::from_bytes(&[b; 32])
    }

    #[test]
    fn entrada_assinada_verifica() {
        let path = tmp("assina");
        let mut log = Log::load(&path).unwrap();
        let e = log
            .append_local(&key(1), [0u8; 24], vec![9, 9, 9], 1000)
            .unwrap();
        e.verify().expect("a propria entrada devia verificar");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn adulterar_o_conteudo_invalida_a_assinatura() {
        let path = tmp("adultera");
        let mut log = Log::load(&path).unwrap();
        let mut e = log
            .append_local(&key(1), [0u8; 24], vec![1, 2, 3], 1000)
            .unwrap();
        e.ciphertext = HEXLOWER.encode(&[4u8, 5, 6]);
        assert!(
            e.verify().is_err(),
            "mexer no ciphertext tem de partir a assinatura"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn merge_rejeita_entrada_invalida() {
        let path_a = tmp("rejeita-a");
        let path_b = tmp("rejeita-b");
        let mut origem = Log::load(&path_a).unwrap();
        let mut e = origem
            .append_local(&key(1), [0u8; 24], vec![1, 2, 3], 1000)
            .unwrap();
        e.ts_ms = 9999; // altera um campo coberto pelo hash

        let mut destino = Log::load(&path_b).unwrap();
        assert_eq!(
            destino.merge(vec![e]).unwrap(),
            0,
            "entrada adulterada nao entra"
        );
        assert_eq!(destino.len(), 0);
        let _ = std::fs::remove_file(&path_a);
        let _ = std::fs::remove_file(&path_b);
    }

    #[test]
    fn merge_deduplica() {
        let path_a = tmp("dedup-a");
        let path_b = tmp("dedup-b");
        let mut origem = Log::load(&path_a).unwrap();
        let e = origem
            .append_local(&key(1), [0u8; 24], vec![1], 1000)
            .unwrap();

        let mut destino = Log::load(&path_b).unwrap();
        assert_eq!(destino.merge(vec![e.clone()]).unwrap(), 1);
        assert_eq!(
            destino.merge(vec![e]).unwrap(),
            0,
            "a mesma entrada nao conta duas vezes"
        );
        assert_eq!(destino.len(), 1);
        let _ = std::fs::remove_file(&path_a);
        let _ = std::fs::remove_file(&path_b);
    }

    #[test]
    fn peers_convergem_para_a_mesma_ordem() {
        // A propriedade que sustenta o desenho: dois peers que recebem as mesmas entradas por
        // ordens diferentes tem de acabar com exatamente o mesmo historico.
        let pa = tmp("conv-a");
        let pb = tmp("conv-b");
        let pfonte = tmp("conv-fonte");

        let mut fonte = Log::load(&pfonte).unwrap();
        let e1 = fonte
            .append_local(&key(1), [1u8; 24], vec![1], 3000)
            .unwrap();
        let e2 = fonte
            .append_local(&key(2), [2u8; 24], vec![2], 1000)
            .unwrap();
        // Empate de timestamp de proposito: o desempate por hash tem de resolver.
        let e3 = fonte
            .append_local(&key(3), [3u8; 24], vec![3], 1000)
            .unwrap();

        let mut a = Log::load(&pa).unwrap();
        a.merge(vec![e1.clone(), e2.clone(), e3.clone()]).unwrap();

        let mut b = Log::load(&pb).unwrap();
        b.merge(vec![e3, e1, e2]).unwrap(); // ordem de chegada diferente

        let ha: Vec<String> = a.ordered().iter().map(|e| e.hash_hex().unwrap()).collect();
        let hb: Vec<String> = b.ordered().iter().map(|e| e.hash_hex().unwrap()).collect();
        assert_eq!(ha, hb, "peers com as mesmas entradas tem de convergir");
        assert_eq!(ha.len(), 3);

        for p in [pa, pb, pfonte] {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn recarregar_do_disco_preserva_o_historico() {
        let path = tmp("persiste");
        let mut log = Log::load(&path).unwrap();
        log.append_local(&key(1), [0u8; 24], vec![1], 1000).unwrap();
        log.append_local(&key(1), [1u8; 24], vec![2], 2000).unwrap();
        drop(log);

        let recarregado = Log::load(&path).unwrap();
        assert_eq!(
            recarregado.len(),
            2,
            "o historico tem de sobreviver ao reinicio"
        );
        let _ = std::fs::remove_file(&path);
    }
    #[test]
    fn relogio_atrasado_nao_inverte_a_resposta() {
        // O bug que isto fecha: a pergunta e escrita por quem tem o relogio certo, a resposta
        // por quem o tem cinco segundos atrasado. Ordenando por relogio de parede, a resposta
        // aparecia ANTES da pergunta -- e nunca se via em testes locais, porque ai as duas
        // maquinas sao a mesma. Entre os EUA e o Brasil, ve-se.
        let path = tmp("relogio");
        let mut log = Log::load(&path).unwrap();
        let pergunta = log
            .append_local(&key(1), [0u8; 24], vec![1], 10_000)
            .unwrap();
        let resposta = log
            .append_local(&key(2), [1u8; 24], vec![2], 5_000)
            .unwrap();

        assert_eq!(
            resposta.prev,
            pergunta.hash_hex().unwrap(),
            "a resposta tem de apontar para a pergunta"
        );
        assert!(
            resposta.ts_ms < pergunta.ts_ms,
            "o cenario so tem valor com o relogio atrasado"
        );

        let ordem: Vec<String> = log
            .ordered()
            .iter()
            .map(|e| e.hash_hex().unwrap())
            .collect();
        assert_eq!(
            ordem[0],
            pergunta.hash_hex().unwrap(),
            "a pergunta tem de vir primeiro apesar do carimbo maior"
        );
        assert_eq!(ordem[1], resposta.hash_hex().unwrap());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn relogios_em_sintonia_dao_ordem_de_parede() {
        // O outro lado da moeda: quando nao ha desvio, o resultado tem de ser exatamente a
        // ordem cronologica. O relogio logico nao pode inventar uma ordem estranha no caso normal.
        let path = tmp("sintonia");
        let mut log = Log::load(&path).unwrap();
        let a = log
            .append_local(&key(1), [0u8; 24], vec![1], 1_000)
            .unwrap();
        let b = log
            .append_local(&key(2), [1u8; 24], vec![2], 2_000)
            .unwrap();
        let c = log
            .append_local(&key(3), [2u8; 24], vec![3], 3_000)
            .unwrap();

        let ordem: Vec<String> = log
            .ordered()
            .iter()
            .map(|e| e.hash_hex().unwrap())
            .collect();
        let esperado: Vec<String> = [&a, &b, &c].iter().map(|e| e.hash_hex().unwrap()).collect();
        assert_eq!(ordem, esperado);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn orfas_deteta_pai_em_falta() {
        // E isto que torna o `prev` verificavel em vez de decorativo.
        let pa = tmp("orfa-a");
        let pb = tmp("orfa-b");
        let mut origem = Log::load(&pa).unwrap();
        let primeira = origem
            .append_local(&key(1), [0u8; 24], vec![1], 1_000)
            .unwrap();
        let segunda = origem
            .append_local(&key(1), [1u8; 24], vec![2], 2_000)
            .unwrap();

        // O destino recebe SO a segunda: o pai dela nunca chegou.
        let mut destino = Log::load(&pb).unwrap();
        destino.merge(vec![segunda.clone()]).unwrap();
        assert_eq!(
            destino.orfas(),
            vec![segunda.hash_hex().unwrap()],
            "com o pai em falta, a entrada e orfa"
        );

        // Quando o pai chega, o buraco fecha sozinho.
        destino.merge(vec![primeira]).unwrap();
        assert!(
            destino.orfas().is_empty(),
            "com o pai presente ja nao ha orfas"
        );

        for f in [pa, pb] {
            let _ = std::fs::remove_file(f);
        }
    }

    #[test]
    fn primeira_entrada_nao_e_orfa() {
        // Uma raiz aponta para o hash zero, e isso nao e um buraco.
        let path = tmp("raiz");
        let mut log = Log::load(&path).unwrap();
        log.append_local(&key(1), [0u8; 24], vec![1], 1_000)
            .unwrap();
        assert!(log.orfas().is_empty());
        let _ = std::fs::remove_file(&path);
    }
}
