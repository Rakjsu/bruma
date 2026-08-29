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

/// Uma entrada do log: o que vai para o disco e para a rede, tal e qual.
///
/// # NÃO SE ACRESCENTAM CAMPOS AQUI
///
/// Campos novos vão **dentro do cifrado**, na `Carga` — nunca neste struct. A razão é que o
/// `ciphertext` é o último campo do [`Entry::canonical`] e portanto **está coberto pela
/// assinatura**: tudo o que cresce lá dentro (anexos, respostas, reacções, editar, apagar) já
/// está protegido de graça, sem tocar em nada disto.
///
/// Um campo acrescentado a este struct, esse, NÃO entraria no `canonical()` — logo não ficaria
/// coberto pela assinatura, e poderia ser alterado por quem retransmite sem que ninguém desse
/// por isso. É por isso que existe o `deny_unknown_fields` abaixo: um campo que uma versão
/// futura acrescente faz a entrada ser RECUSADA, em vez de aceite com uma parte por verificar.
///
/// Recusar é seguro aqui porque o `Log::load` salta a entrada, conta-a e guarda-a em
/// `.rejeitadas` em vez de matar o log; a ligação não cai por causa disso; e a interface diz
/// que o outro lado tem outra versão. Sem essas três, isto trocaria um silêncio por uma
/// ligação partida.
///
/// Se um dia um campo tiver mesmo de estar em claro — o único candidato conhecido é a rotação
/// de chave de sala —, não se acrescenta aqui: sobe-se um byte de versão e faz-se um
/// `canonical_v2` a coexistir com o v1. Ver o plano de transição no cérebro
/// (`bruma-plano-do-hash`).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
    ///
    /// # ISTO NÃO SE MUDA
    ///
    /// O hash que sai daqui é a IDENTIDADE da entrada: é a chave do mapa, é o `prev` que forma
    /// a cadeia, e é o que a assinatura cobre. Mudar esta função — reordenar, acrescentar,
    /// «arrumar» — faz todas as entradas em disco passarem a ter outro hash, a `verify()` de
    /// cada uma falhar, e o `load` saltá-las uma a uma: **o histórico inteiro desaparece do
    /// ecrã em silêncio**, e a app arranca com salas vazias como se nada fosse.
    ///
    /// O teste `a_disposicao_do_canonical_esta_fixada` existe para isso não acontecer sem
    /// alguém decidir que quer que aconteça.
    ///
    /// Para crescer, ver a nota do [`Entry`]: campos novos vão dentro do cifrado.
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
    /// Linhas que não se conseguiram ler ou cuja assinatura não bateu, ao carregar. Um só
    /// byte trocado no meio do ficheiro deixava de custar uma sessão e passava a custar a
    /// sala inteira. Agora salta-se a linha má, conta-se aqui, e as boas à volta sobrevivem.
    ilegiveis: usize,
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
                let log = Log {
                    entries,
                    path,
                    ilegiveis: 0,
                };
                log.reescrever()?;
                return Ok(log);
            }
            let total = raw.lines().count();
            let mut ilegiveis = 0usize;
            let mut rejeitadas: Vec<&str> = Vec::new();
            for (n, linha) in raw.lines().enumerate() {
                if linha.trim().is_empty() {
                    continue;
                }
                let e: Entry = match serde_json::from_str(linha) {
                    Ok(e) => e,
                    Err(_) => {
                        // A ÚLTIMA linha cortada é a que estava a ser escrita — normal, ignora-se
                        // em silêncio. Uma linha ilegível no MEIO já não mata o ficheiro: um log
                        // é um conjunto de entradas independentes, e não há razão para uma
                        // envenenar as outras. Salta-se, conta-se, e guarda-se de lado.
                        if n + 1 == total {
                            break;
                        }
                        ilegiveis += 1;
                        rejeitadas.push(linha);
                        continue;
                    }
                };
                // A assinatura tem de bater mesmo vinda do disco. Uma que não bate é ruído —
                // salta como o `merge` já faz, em vez de abortar o log inteiro.
                if e.verify().is_err() {
                    ilegiveis += 1;
                    rejeitadas.push(linha);
                    continue;
                }
                let Ok(h) = e.hash_hex() else {
                    ilegiveis += 1;
                    rejeitadas.push(linha);
                    continue;
                };
                entries.insert(h, e);
            }
            // As linhas rejeitadas não se deitam fora: ficam num ficheiro ao lado, para quem
            // souber ler JSON as poder recuperar, e para não serem lixo invisível.
            if !rejeitadas.is_empty() {
                let destino = path.with_extension("rejeitadas");
                let _ = std::fs::write(
                    &destino,
                    rejeitadas.join(
                        "
",
                    ),
                );
                eprintln!(
                    "[dados] {} entrada(s) de {} não se leram em {}; guardadas em {}",
                    ilegiveis,
                    total,
                    path.display(),
                    destino.display()
                );
            }
            return Ok(Log {
                entries,
                path,
                ilegiveis,
            });
        }
        Ok(Log {
            entries,
            path,
            ilegiveis: 0,
        })
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

    /// Se este autor alguma vez escreveu aqui.
    ///
    /// O `author` de cada entrada está em CLARO — é a chave pública de quem assinou, e é
    /// também o endereço de rede dessa pessoa. Logo dá para saber quem pertence a esta sala
    /// sem decifrar uma única mensagem, e sem o custo do `ordered()`, que ordena e recalcula
    /// os hashes todos.
    pub fn escreveu(&self, autor: &str) -> bool {
        self.entries.values().any(|e| e.author == autor)
    }

    /// Quantas entradas não se conseguiram ler no último `load`. Ver [`Log::ilegiveis`].
    pub fn ilegiveis(&self) -> usize {
        self.ilegiveis
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
        // GRAVAR PRIMEIRO, inserir depois.
        //
        // Estava ao contrário: inseria no mapa e só depois anexava. Se a escrita falhasse
        // (disco cheio, pasta só de leitura, o antivírus a segurar o ficheiro), a entrada
        // ficava na memória mas não no disco — aparecia no ecrã, entrava nos `provados`, e o
        // `head()` passava a apontar para um hash que não existe em disco, portanto a mensagem
        // SEGUINTE nascia com um `prev` órfão permanente. O sintoma para quem usa é «as
        // mensagens de ontem desapareceram quando reabri», sem um erro a ligar as duas coisas.
        let hash = e.hash_hex()?;
        self.anexar(std::slice::from_ref(&e))?;
        self.entries.insert(hash, e.clone());
        Ok(e)
    }

    /// Devolve quantas entradas eram novas. Entradas inválidas são rejeitadas, não confiadas.
    pub fn merge(&mut self, incoming: Vec<Entry>) -> Result<usize> {
        // Determinar quais são novas SEM as inserir, gravar o lote, e só então pô-las no
        // mapa. Estava a inserir primeiro e a anexar no fim: se a escrita falhasse, as
        // entradas ficavam na memória e nunca chegavam ao disco — desapareciam ao fechar a
        // app, e a contagem devolvida não distinguia isso de sucesso.
        let mut novas = Vec::new();
        let mut vistos = std::collections::HashSet::new();
        for e in incoming {
            if e.verify().is_err() {
                eprintln!("  [!] entrada rejeitada: assinatura inválida");
                continue;
            }
            let h = e.hash_hex()?;
            // Já no disco, ou repetida dentro deste mesmo lote.
            if self.entries.contains_key(&h) || !vistos.insert(h) {
                continue;
            }
            novas.push(e);
        }
        // Uma gravação para o lote todo, e não uma por entrada: quando alguém entra e traz
        // mil mensagens de histórico, a diferença é entre um write e mil. E se falhar, o `?`
        // sobe o erro ANTES de o mapa mudar — o chamador vê a falha, não um sucesso falso.
        self.anexar(&novas)?;
        for e in &novas {
            self.entries.insert(e.hash_hex()?, e.clone());
        }
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
    fn linha_partida_no_meio_e_saltada_e_nao_mata() {
        // Esta decisão INVERTEU-SE (backlog #11): uma linha partida no meio deixou de matar
        // o log inteiro. Um log é um conjunto de entradas independentes; não há razão para
        // uma envenenar as outras. Salta-se, conta-se, e as boas à volta sobrevivem.
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

        let log = Log::load(&path).expect("não pode recusar o log inteiro por uma linha");
        assert_eq!(log.len(), 2, "as duas boas sobrevivem");
        assert_eq!(log.ilegiveis(), 1, "e a má é contada, não escondida");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("rejeitadas"));
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

    /// A DISPOSIÇÃO do `canonical()` está fixada, e mudá-la falha aqui.
    ///
    /// O teste constrói os bytes à mão, segundo a disposição documentada — `author ‖ ts_ms ‖
    /// prev ‖ nonce ‖ ciphertext`, o `ts_ms` em big-endian — e exige que o `hash()` concorde.
    /// Não é uma fotografia do que o código faz: é a ESPECIFICAÇÃO escrita à parte, com o
    /// código a ser verificado contra ela. Se alguém reordenar, acrescentar um campo ou trocar
    /// o big-endian, isto falha e diz porquê.
    ///
    /// Porque é que vale a pena: mudar o `canonical()` faz todas as entradas em disco passarem
    /// a ter outro hash, as assinaturas deixarem de conferir, e o `load` saltá-las uma a uma —
    /// o histórico desaparece **em silêncio**. A correcção que tornou o log resiliente a uma
    /// linha estragada tornou este erro mais silencioso, não menos.
    #[test]
    fn a_disposicao_do_canonical_esta_fixada() {
        let autor = [0xABu8; 32];
        let prev = [0xCDu8; 32];
        let nonce = [0xEFu8; 24];
        let ct: Vec<u8> = vec![1, 2, 3, 4, 5];
        let ts: u64 = 0x0102_0304_0506_0708;

        let e = Entry {
            author: HEXLOWER.encode(&autor),
            ts_ms: ts,
            prev: HEXLOWER.encode(&prev),
            nonce: HEXLOWER.encode(&nonce),
            ciphertext: HEXLOWER.encode(&ct),
            sig: String::new(),
        };

        // A disposição, escrita aqui à mão e não tirada do `canonical()`.
        let mut esperado = Vec::new();
        esperado.extend_from_slice(&autor);
        esperado.extend_from_slice(&ts.to_be_bytes());
        esperado.extend_from_slice(&prev);
        esperado.extend_from_slice(&nonce);
        esperado.extend_from_slice(&ct);
        assert_eq!(
            e.canonical().unwrap(),
            esperado,
            "a disposição do canonical() mudou — ver a nota na função antes de seguir"
        );

        // E o hash que dela sai, preso a um valor. Apanha até uma mudança que alterasse os
        // dois lados do assert acima ao mesmo tempo.
        assert_eq!(
            e.hash_hex().unwrap(),
            "37d7c975e7a9d011cf46e8b2c089f40239bd992c97c88fdee8b5f93325c172be",
            "o hash canónico mudou — o histórico de toda a gente deixaria de verificar"
        );
    }

    /// E o que a assinatura COBRE também está preso — não só a disposição dos bytes.
    ///
    /// O teste acima prende o `canonical()` e o algoritmo do resumo. Não prendia o
    /// ARGUMENTO que vai ao `sign`. Trocar `signing.sign(&e.hash()?)` por
    /// `signing.sign(&e.canonical()?)` no `append_local` e a mesma troca no `verify()`
    /// deixava a suite inteira verde — as duas metades continuam a concordar uma com a
    /// outra — e apagava o histórico de toda a gente na actualização seguinte, porque as
    /// assinaturas já gravadas foram feitas sobre a outra mensagem.
    ///
    /// Por isso este teste NÃO assina nada por sua conta: manda o `append_local` fazê-lo, e
    /// prende o resultado. A chave é determinística (`key(1)` é `[1; 32]`) e o Ed25519 é
    /// determinístico por norma, portanto a assinatura de uma entrada conhecida é um valor
    /// fixo. Fixá-lo é fixar o caminho de escrita inteiro: o `head()` de um log vazio, a
    /// montagem do `Entry`, a disposição, o resumo, e o que se assina.
    #[test]
    fn o_que_a_assinatura_cobre_esta_fixado() {
        let caminho = tmp("assinatura-fixada");
        let mut log = Log::load(&caminho).unwrap();
        let e = log
            .append_local(
                &key(1),
                [0xEF; 24],
                vec![1, 2, 3, 4, 5],
                0x0102_0304_0506_0708,
            )
            .unwrap();

        // Um log vazio: o `prev` é o hash-zero. Se isto mudar, tudo o resto abaixo muda.
        assert_eq!(
            e.prev,
            HEXLOWER.encode(&ZERO_HASH),
            "o prev de um log vazio"
        );

        assert_eq!(
            e.hash_hex().unwrap(),
            "e99714b702980e788201811cf29fc86c6d949b0a8cd352c31cbfbcfc4982298c",
            "o hash da entrada escrita pelo caminho de produção mudou"
        );
        assert_eq!(
            e.sig, "3b8d01d968db6e28923fd8f210f91fce4c9b615a3d035e8ad7c6dc61f1f98dc121f78a7288c6348dd053bf2850d6f304b8baa09590dab08bd9b2e93335dd8502",
            "a MENSAGEM assinada mudou — as assinaturas já gravadas deixariam de conferir"
        );

        // E continua a verificar, que é o outro lado da mesma moeda.
        e.verify()
            .expect("a entrada que acabámos de escrever tem de verificar");
        let _ = std::fs::remove_file(&caminho);
    }

    /// Um campo que uma versão futura acrescente ao `Entry` é RECUSADO, não ignorado.
    ///
    /// Sem o `deny_unknown_fields`, o serde ignorava-o: a entrada parseava, o `canonical()`
    /// calculava-se sem ele, a assinatura conferia, e a entrada era aceite — com um campo que
    /// ninguém verificou e que qualquer retransmissor podia ter alterado. Uma falha de
    /// segurança que se apresenta como sucesso.
    #[test]
    fn um_campo_novo_no_entry_e_recusado() {
        let bom =
            br#"{"author":"aa","ts_ms":1,"prev":"bb","nonce":"cc","ciphertext":"dd","sig":"ee"}"#;
        assert!(
            serde_json::from_slice::<Entry>(bom).is_ok(),
            "uma entrada normal tem de continuar a ser lida"
        );

        // O que uma v0.19 mandaria se acrescentasse um campo ao Entry.
        let futuro = br#"{"author":"aa","ts_ms":1,"prev":"bb","nonce":"cc","ciphertext":"dd","sig":"ee","chave_usada":"f0"}"#;
        assert!(
            serde_json::from_slice::<Entry>(futuro).is_err(),
            "um campo por verificar tem de ser recusado, não aceite em silêncio"
        );
    }

    /// Uma linha estragada no meio não mata o histórico à volta.
    ///
    /// Antes, um byte trocado numa linha do meio fazia o `load` devolver `Err`, o servidor era
    /// posto de lado, e a chave apagada a seguir: um bit custava a sala inteira. Agora a linha
    /// má é saltada, contada, e guardada em `.rejeitadas`; as boas entram.
    #[test]
    fn linha_estragada_nao_mata_o_log() {
        let path = tmp("linha-estragada");
        {
            let mut log = Log::load(&path).unwrap();
            for i in 0..5u8 {
                log.append_local(&key(1), [0u8; 24], vec![i], 1_000 + i as u64)
                    .unwrap();
            }
        }
        // Estragar a linha do MEIO (a 3.ª de 5), deixando as outras intactas.
        let raw = std::fs::read_to_string(&path).unwrap();
        let mut linhas: Vec<String> = raw.lines().map(|l| l.to_string()).collect();
        assert_eq!(linhas.len(), 5, "cinco entradas");
        linhas[2] = "{isto nao e json valido".into();
        std::fs::write(
            &path,
            linhas.join(
                "
",
            ),
        )
        .unwrap();

        let log = Log::load(&path).unwrap();
        assert_eq!(log.len(), 4, "as quatro boas tinham de sobreviver");
        assert_eq!(log.ilegiveis(), 1, "e a estragada tinha de ser contada");
        assert!(
            path.with_extension("rejeitadas").exists(),
            "a linha rejeitada tinha de ficar guardada de lado"
        );

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("rejeitadas"));
    }

    /// Se a escrita falha, a entrada NÃO fica na memória — o erro sobe e o `len()` não muda.
    ///
    /// Sem isto, uma entrada aparecia no ecrã e nunca chegava ao disco: desaparecia ao
    /// fechar a app, e o `head()` ficava a apontar para um hash órfão. Força-se a falha
    /// marcando o ficheiro do log como só-leitura, que faz o `OpenOptions::append().open()`
    /// dar «access denied» em Windows.
    #[test]
    fn escrita_falhada_nao_deixa_entrada_na_memoria() {
        let path = tmp("escrita-falhada");
        let mut log = Log::load(&path).unwrap();
        log.append_local(&key(1), [0u8; 24], vec![1], 1_000)
            .unwrap();
        assert_eq!(log.len(), 1, "a primeira entrou");

        // Ficheiro só-leitura: a próxima escrita tem de falhar.
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&path, perms).unwrap();

        // append_local: o erro sobe e o mapa não cresce.
        let r = log.append_local(&key(1), [0u8; 24], vec![2], 2_000);
        assert!(r.is_err(), "a escrita bloqueada tinha de dar erro");
        assert_eq!(log.len(), 1, "a entrada falhada não pode ficar na memória");

        // merge: idem. Uma entrada nova, válida, que não se consegue gravar.
        let mut fonte = Log::load(tmp("escrita-falhada-fonte")).unwrap();
        let e = fonte
            .append_local(&key(2), [0u8; 24], vec![3], 3_000)
            .unwrap();
        let r = log.merge(vec![e]);
        assert!(r.is_err(), "o merge bloqueado tinha de dar erro");
        assert_eq!(
            log.len(),
            1,
            "a entrada do merge falhado não pode ficar na memória"
        );

        // Repor a escrita para poder apagar. O `set_readonly(false)` é o que interessa aqui
        // (é um teste, não um controlo de acesso real), por isso silencia-se o lint.
        #[allow(clippy::permissions_set_readonly_false)]
        {
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_readonly(false);
            let _ = std::fs::set_permissions(&path, perms);
        }
        let _ = std::fs::remove_file(&path);
    }
}
