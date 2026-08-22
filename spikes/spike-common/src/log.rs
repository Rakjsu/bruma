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
    /// Lê o registo do disco.
    ///
    /// O formato é uma entrada por linha. A ordem no ficheiro não significa nada — a ordem
    /// de leitura sai do relógio lógico, no `ordered()` —, e é isso que permite gravar por
    /// acrescento em vez de reescrever tudo.
    ///
    /// **Uma linha por gravar não invalida o resto.** Se a última linha estiver cortada
    /// (o computador desligou-se a meio de a escrever), perde-se essa e mais nada. É a
    /// diferença entre perder uma mensagem e perder o histórico todo, e num programa sem
    /// servidor não há de onde o recuperar.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let mut entries = BTreeMap::new();
        if path.exists() {
            let raw = std::fs::read_to_string(&path)?;
            // Ficheiros do formato antigo eram um array JSON indentado. Lêem-se uma vez e
            // ficam convertidos na gravação seguinte.
            if raw.trim_start().starts_with('[') {
                for e in serde_json::from_str::<Vec<Entry>>(&raw)? {
                    e.verify()?;
                    entries.insert(e.hash_hex()?, e);
                }
                let log = Log { entries, path };
                log.reescrever()?;
                return Ok(log);
            }
            for (n, linha) in raw.lines().enumerate() {
                if linha.trim().is_empty() {
                    continue;
                }
                let e: Entry = match serde_json::from_str(linha) {
                    Ok(e) => e,
                    Err(erro) => {
                        // Só a ÚLTIMA linha pode estar cortada: é a que estava a ser
                        // escrita. Uma linha partida no meio é corrupção a sério e não se
                        // finge que não é.
                        if n + 1 == raw.lines().count() {
                            break;
                        }
                        return Err(anyhow!("linha {} do registo ilegível: {erro}", n + 1));
                    }
                };
                // Recusa entradas adulteradas mesmo vindas do disco.
                e.verify()?;
                entries.insert(e.hash_hex()?, e);
            }
        }
        Ok(Log { entries, path })
    }

    /// Acrescenta entradas ao fim do ficheiro.
    ///
    /// Antes gravava-se o registo inteiro a cada mensagem. Com mil mensagens isso são mil
    /// reescritas do ficheiro todo, e a milésima escreve mil entradas para acrescentar uma
    /// — e, pior do que ser lento, o `write` trunca antes de escrever: um corte de energia
    /// no instante errado apagava o histórico inteiro.
    fn anexar(&self, novas: &[Entry]) -> Result<()> {
        use std::io::Write;
        if novas.is_empty() {
            return Ok(());
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        let mut buf = String::new();
        for e in novas {
            buf.push_str(&serde_json::to_string(e)?);
            buf.push('\n');
        }
        f.write_all(buf.as_bytes())?;
        // Sem isto, o "já gravei" é uma promessa do sistema operativo e não do disco.
        f.sync_data()?;
        Ok(())
    }

    /// Reescreve o ficheiro do zero. Só se usa na conversão do formato antigo.
    ///
    /// Passa por um ficheiro temporário e só depois toma o lugar do outro: enquanto o novo
    /// não estiver inteiro, o antigo continua a ser o bom. Escrever por cima do próprio
    /// deixaria uma janela em que não existe nenhum dos dois.
    fn reescrever(&self) -> Result<()> {
        use std::io::Write;
        let temporario = self.path.with_extension("novo");
        {
            let mut f = std::fs::File::create(&temporario)?;
            for e in self.entries.values() {
                f.write_all(serde_json::to_string(e)?.as_bytes())?;
                f.write_all(b"\n")?;
            }
            f.sync_data()?;
        }
        std::fs::rename(&temporario, &self.path)?;
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
        self.anexar(std::slice::from_ref(&e))?;
        Ok(e)
    }

    /// Devolve quantas entradas eram novas. Entradas inválidas são rejeitadas, não confiadas.
    pub fn merge(&mut self, incoming: Vec<Entry>) -> Result<usize> {
        let mut novas = Vec::new();
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
                novas.push(e.clone());
                slot.insert(e);
            }
        }
        // Uma gravação para o lote todo, e não uma por entrada: quando alguém entra e traz
        // mil mensagens de histórico, a diferença é entre um write e mil.
        self.anexar(&novas)?;
        Ok(novas.len())
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

    /// A propriedade que interessa quando falta a luz: uma linha por gravar custa essa
    /// mensagem, e mais nada. O formato antigo reescrevia o ficheiro todo a cada mensagem,
    /// e um corte no instante errado levava o historico inteiro -- que, sem servidor, nao
    /// se recupera de lado nenhum.
    #[test]
    fn linha_cortada_a_meio_da_gravacao_custa_uma_mensagem_e_nao_o_historico() {
        let path = tmp("corte");
        {
            let mut log = Log::load(&path).unwrap();
            for i in 0..5u64 {
                log.append_local(&key(1), [0u8; 24], vec![i as u8], 1000 + i)
                    .unwrap();
            }
        }
        // Simula o computador a desligar-se a meio de escrever a sexta.
        let mut bruto = std::fs::read_to_string(&path).unwrap();
        bruto.push_str("{\"author\":\"ab\",\"ts_m");
        std::fs::write(&path, &bruto).unwrap();

        let log = Log::load(&path).unwrap();
        assert_eq!(log.len(), 5, "as cinco que ficaram gravadas continuam la");
        let _ = std::fs::remove_file(&path);
    }

    /// Uma linha partida no MEIO nao e um corte de energia -- e corrupcao. Ai nao se finge
    /// que esta tudo bem, porque continuar a acrescentar por cima de um ficheiro estragado
    /// so espalha o estrago.
    #[test]
    fn linha_partida_no_meio_e_erro_e_nao_silencio() {
        let path = tmp("corrompido");
        {
            let mut log = Log::load(&path).unwrap();
            for i in 0..3u64 {
                log.append_local(&key(1), [0u8; 24], vec![i as u8], 1000 + i)
                    .unwrap();
            }
        }
        let bruto = std::fs::read_to_string(&path).unwrap();
        let mut linhas: Vec<&str> = bruto.lines().collect();
        linhas[1] = "{isto nao e json";
        std::fs::write(
            &path,
            linhas.join(
                "
",
            ) + "
",
        )
        .unwrap();

        assert!(Log::load(&path).is_err(), "tem de recusar, nao ignorar");
        let _ = std::fs::remove_file(&path);
    }

    /// Quem ja tinha o Bruma instalado tem ficheiros no formato antigo. Abrir a app nova
    /// nao pode perder nada.
    #[test]
    fn le_o_formato_antigo_e_converte_sem_perder_nada() {
        let path_origem = tmp("antigo-origem");
        let path = tmp("antigo");
        let mut origem = Log::load(&path_origem).unwrap();
        let entradas: Vec<Entry> = (0..4u64)
            .map(|i| {
                origem
                    .append_local(&key(1), [0u8; 24], vec![i as u8], 1000 + i)
                    .unwrap()
            })
            .collect();

        // O formato antigo: um array JSON indentado.
        std::fs::write(&path, serde_json::to_string_pretty(&entradas).unwrap()).unwrap();

        let log = Log::load(&path).unwrap();
        assert_eq!(log.len(), 4, "nenhuma entrada se perde na conversao");

        // E ficou convertido: o ficheiro deixa de comecar por um parentese reto.
        let agora = std::fs::read_to_string(&path).unwrap();
        assert!(!agora.trim_start().starts_with('['), "devia ter reescrito");
        assert_eq!(agora.lines().count(), 4, "uma entrada por linha");

        // E continua a abrir bem no formato novo.
        assert_eq!(Log::load(&path).unwrap().len(), 4);
        let _ = std::fs::remove_file(&path_origem);
        let _ = std::fs::remove_file(&path);
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
