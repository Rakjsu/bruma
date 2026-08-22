//! Traduzir o MP4 fragmentado do Windows para o dialeto que o navegador aceita.
//!
//! # O problema, dito pelo próprio navegador
//!
//! ```text
//! CHUNK_DEMUXER_ERROR_APPEND_FAILED: Failure parsing MP4:
//! TFHD base-data-offset not allowed by MSE.
//! ```
//!
//! O sink do Media Foundation escreve cada fragmento com endereçamento **absoluto**: a
//! caixa `tfhd` traz um `base_data_offset` que aponta para uma posição no ficheiro. Faz
//! todo o sentido num ficheiro. Mas o Media Source Extensions exige endereçamento
//! **relativo ao `moof`** — porque numa transmissão não existe "posição no ficheiro": o
//! espectador pode ter entrado a meio, e um fragmento tem de ser decifrável sozinho.
//!
//! Não é um defeito de nenhum dos dois. São dois mundos com pressupostos diferentes, e
//! alguém tem de traduzir. É o que este módulo faz, e só isso.
//!
//! # A tradução
//!
//! Em cada `moof`:
//!
//!  - tira-se o `base_data_offset` do `tfhd` (8 bytes) e troca-se a flag que o anuncia
//!    (`0x000001`) pela `default-base-is-moof` (`0x020000`), que é a que o MSE quer;
//!  - o `data_offset` do `trun` passava a contar a partir do `base_data_offset`; passa a
//!    contar a partir do início do `moof`. Como o `moof` encolheu 8 bytes ao perder o
//!    campo, o novo valor desconta-os também;
//!  - os tamanhos do `tfhd`, do `traf` e do `moof` descem 8.
//!
//! # E a peça que faltava: o `tfdt`
//!
//! Corrigido o endereçamento, o navegador continuava a recusar — e o mesmo vídeo tocava
//! sem uma queixa num `<video>` comum, o que provou que o problema não era o ficheiro.
//! Faltava o `tfdt`, a caixa que diz em que instante cada fragmento começa. Num ficheiro
//! ninguém precisa dela: lê-se do princípio e conta-se. Numa transmissão é obrigatória,
//! porque cada fragmento tem de se situar sozinho no tempo. O Media Foundation não a
//! escreve; aqui acrescenta-se, somando as durações das amostras dos fragmentos anteriores.
//!
//! Se alguma coisa não bater certo — uma caixa a faltar, um tamanho impossível — não se
//! adivinha: devolve-se `None` e o fragmento segue como estava. Um fragmento que o
//! navegador recusa é um problema visível; um fragmento corrompido por um palpite nosso é
//! um problema que aparece três horas depois e ninguém liga a este ficheiro.

/// Com `--autoteste`, escreve as primeiras caixas que saem. Quando o navegador diz só
/// "stream parsing failed", saber a sequência real poupa uma tarde de palpites.
pub static VER_CAIXAS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Um cabeçalho de caixa: 4 bytes de tamanho, 4 de tipo.
const CABECALHO: usize = 8;

const TFHD_BASE_DATA_OFFSET: u32 = 0x00_0001;
const TFHD_DEFAULT_BASE_IS_MOOF: u32 = 0x02_0000;
const TRUN_DATA_OFFSET: u32 = 0x00_0001;
const TRUN_FIRST_SAMPLE_FLAGS: u32 = 0x00_0004;
const TRUN_SAMPLE_DURATION: u32 = 0x00_0100;
const TRUN_SAMPLE_SIZE: u32 = 0x00_0200;
const TRUN_SAMPLE_FLAGS: u32 = 0x00_0400;
const TRUN_SAMPLE_CTS: u32 = 0x00_0800;

fn u32_em(d: &[u8], i: usize) -> Option<u32> {
    d.get(i..i + 4)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

fn u64_em(d: &[u8], i: usize) -> Option<u64> {
    d.get(i..i + 8)
        .map(|b| u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
}

fn tipo_em(d: &[u8], i: usize) -> Option<&[u8]> {
    d.get(i + 4..i + 8)
}

/// Percorre as caixas filhas a partir de `inicio` e devolve onde começa a primeira do
/// tipo pedido, e o tamanho dela.
fn filha(d: &[u8], mut i: usize, fim: usize, tipo: &[u8; 4]) -> Option<(usize, usize)> {
    while i + CABECALHO <= fim {
        let tam = u32_em(d, i)? as usize;
        if tam < CABECALHO || i + tam > fim {
            return None;
        }
        if tipo_em(d, i)? == tipo {
            return Some((i, tam));
        }
        i += tam;
    }
    None
}

/// Devolve o tamanho da primeira caixa completa a partir de `i`, ou `None` se ainda não
/// chegou inteira.
pub fn tamanho_da_caixa(d: &[u8], i: usize) -> Option<usize> {
    if i + CABECALHO > d.len() {
        return None;
    }
    let tam = u32_em(d, i)? as usize;
    // Tamanho 0 significa "até ao fim do ficheiro" e 1 significa "o tamanho vem a
    // seguir, em 64 bits". Nenhum dos dois aparece aqui, e adivinhar seria pior.
    if tam < CABECALHO || i + tam > d.len() {
        return None;
    }
    Some(tam)
}

pub fn e_moof(d: &[u8], i: usize) -> bool {
    tipo_em(d, i) == Some(b"moof")
}

/// O nome da caixa que começa em `i`, para diagnóstico.
pub fn nome_da_caixa(d: &[u8], i: usize) -> String {
    tipo_em(d, i)
        .map(|t| String::from_utf8_lossy(t).to_string())
        .unwrap_or_else(|| "????".into())
}

/// As caixas que o MSE quer ver e mais nenhuma.
///
/// O segmento de inicialização do MSE é `ftyp` + `moov`, e os fragmentos são `moof` +
/// `mdat`. O Media Foundation escreve ainda um `uuid` e um `pdin` pelo meio, que num
/// ficheiro são inofensivos e aqui não têm onde encaixar. Em vez de os deixar passar e
/// esperar que o navegador os ignore, não se enviam: o que não vai não pode ser recusado.
pub fn interessa_ao_navegador(d: &[u8], i: usize) -> bool {
    matches!(
        tipo_em(d, i),
        Some(b"ftyp") | Some(b"moov") | Some(b"moof") | Some(b"mdat") | Some(b"styp")
    )
}

/// O nome do codec, lido do próprio fluxo.
///
/// O `addSourceBuffer` do MSE obriga a declarar o codec, e o navegador **valida** o que se
/// lhe declara contra o segmento de inicialização: se não bater certo, recusa o fluxo
/// inteiro com um "stream parsing failed" que não diz porquê. Custou uma tarde.
///
/// Não se põe uma constante aqui por uma razão simples: o perfil depende de quem
/// codificou. Esta máquina, com a NVIDIA, produz `avc1.42C02A` — Baseline, nível 4.2 —
/// e uma placa Intel ou AMD produzirá outro. Lê-se do `avcC`, que é onde está escrito.
pub fn codec_do_moov(moov: &[u8]) -> Option<String> {
    fn procurar(d: &[u8], mut i: usize, fim: usize, alvo: &[u8; 4]) -> Option<(usize, usize)> {
        while i + CABECALHO <= fim {
            let tam = u32_em(d, i)? as usize;
            if tam < CABECALHO || i + tam > fim {
                return None;
            }
            let t = tipo_em(d, i)?;
            if t == alvo {
                return Some((i, tam));
            }
            // Só se desce por onde o `avc1` pode estar. O `stsd` tem versão, flags e
            // contagem antes das entradas — descer sem saltar esses 8 bytes dá lixo.
            let dentro = match t {
                b"trak" | b"mdia" | b"minf" | b"stbl" => Some(i + CABECALHO),
                b"stsd" => Some(i + CABECALHO + 8),
                _ => None,
            };
            if let Some(ini) = dentro {
                if let Some(r) = procurar(d, ini, i + tam, alvo) {
                    return Some(r);
                }
            }
            i += tam;
        }
        None
    }

    let (avc1, tam) = procurar(moov, CABECALHO, moov.len(), b"avc1")?;
    // A entrada `avc1` tem 78 bytes de campos fixos antes das caixas que a descrevem.
    let (avcc, _) = procurar(moov, avc1 + CABECALHO + 78, avc1 + tam, b"avcC")?;
    // [cabeçalho 8][configurationVersion 1][perfil 1][compatibilidade 1][nível 1]
    let p = moov.get(avcc + CABECALHO + 1..avcc + CABECALHO + 4)?;
    Some(format!("avc1.{:02X}{:02X}{:02X}", p[0], p[1], p[2]))
}

/// Traduz um `moof` para o dialeto do MSE.
///
/// `posicao` é onde este `moof` começa no fluxo original e `tempo` é a soma das durações
/// dos fragmentos anteriores. Devolve a caixa traduzida e a duração deste fragmento, para
/// o chamador poder continuar a contar.
///
/// Devolve `None` quando a caixa não tem a forma esperada — e aí ela segue como estava.
pub fn corrigir_moof(moof: &[u8], posicao: u64, tempo: u64) -> Option<(Vec<u8>, u64)> {
    let fim = moof.len();
    if fim < CABECALHO || tipo_em(moof, 0)? != b"moof" {
        return None;
    }

    let (traf_i, traf_tam) = filha(moof, CABECALHO, fim, b"traf")?;
    let traf_fim = traf_i + traf_tam;
    let (tfhd_i, tfhd_tam) = filha(moof, traf_i + CABECALHO, traf_fim, b"tfhd")?;
    let (trun_i, trun_tam) = filha(moof, traf_i + CABECALHO, traf_fim, b"trun")?;

    let flags_tfhd = u32_em(moof, tfhd_i + CABECALHO)? & 0x00FF_FFFF;
    let tem_base = flags_tfhd & TFHD_BASE_DATA_OFFSET != 0;
    let ja_tem_tfdt = filha(moof, traf_i + CABECALHO, traf_fim, b"tfdt").is_some();
    if !tem_base && ja_tem_tfdt {
        return None; // já está no dialeto certo
    }
    if !tem_base {
        return None; // forma que não se conhece; não se adivinha
    }

    // [cabeçalho 8][versão+flags 4][track_ID 4][base_data_offset 8]
    let base_i = tfhd_i + CABECALHO + 4 + 4;
    let base = u64_em(moof, base_i)?;
    if tfhd_tam < (base_i - tfhd_i) + 8 {
        return None;
    }

    let duracao = duracao_do_trun(moof, trun_i, trun_tam)?;

    // Monta-se de novo, em vez de remendar o original: são três mexidas com tamanhos
    // diferentes (tirar 8, pôr 20, corrigir três caixas) e remendar dava índices a mudar
    // debaixo dos pés.
    let tfdt = caixa_tfdt(tempo);
    let novo_traf_tam = traf_tam - 8 + tfdt.len();
    let novo_moof_tam = fim - 8 + tfdt.len();

    let mut novo = Vec::with_capacity(novo_moof_tam);
    // moof: cabeçalho com o tamanho novo, e tudo até ao traf
    novo.extend_from_slice(&(novo_moof_tam as u32).to_be_bytes());
    novo.extend_from_slice(b"moof");
    novo.extend_from_slice(&moof[CABECALHO..traf_i]);
    // traf: cabeçalho com o tamanho novo
    novo.extend_from_slice(&(novo_traf_tam as u32).to_be_bytes());
    novo.extend_from_slice(b"traf");
    // tfhd: sem o endereço absoluto e com a flag que o MSE quer
    let flags_novas = (flags_tfhd & !TFHD_BASE_DATA_OFFSET) | TFHD_DEFAULT_BASE_IS_MOOF;
    let versao_tfhd = moof[tfhd_i + CABECALHO];
    novo.extend_from_slice(&((tfhd_tam - 8) as u32).to_be_bytes());
    novo.extend_from_slice(b"tfhd");
    novo.extend_from_slice(&(((versao_tfhd as u32) << 24) | flags_novas).to_be_bytes());
    novo.extend_from_slice(&moof[tfhd_i + CABECALHO + 4..base_i]);
    novo.extend_from_slice(&moof[base_i + 8..tfhd_i + tfhd_tam]);
    // tfdt: a peça que faltava
    novo.extend_from_slice(&tfdt);
    // o resto do traf, com o trun já corrigido
    let mut i = tfhd_i + tfhd_tam;
    while i + CABECALHO <= traf_fim {
        let tam = u32_em(moof, i)? as usize;
        if tam < CABECALHO || i + tam > traf_fim {
            return None;
        }
        if i == trun_i {
            novo.extend_from_slice(&corrigir_trun(
                &moof[trun_i..trun_i + trun_tam],
                base,
                posicao,
                tfdt.len(),
            )?);
        } else if tipo_em(moof, i)? != b"tfdt" {
            novo.extend_from_slice(&moof[i..i + tam]);
        }
        i += tam;
    }
    // o que vier depois do traf dentro do moof
    novo.extend_from_slice(&moof[traf_fim..fim]);

    Some((novo, duracao))
}

/// A caixa que diz em que instante este fragmento começa. Versão 1 para o tempo caber em
/// 64 bits: numa transmissão que dure horas, 32 bits dão a volta.
fn caixa_tfdt(tempo: u64) -> Vec<u8> {
    let mut v = 20u32.to_be_bytes().to_vec();
    v.extend_from_slice(b"tfdt");
    v.extend_from_slice(&0x0100_0000u32.to_be_bytes()); // versão 1, sem flags
    v.extend_from_slice(&tempo.to_be_bytes());
    v
}

/// Quanto tempo dura este fragmento, somando as amostras.
fn duracao_do_trun(d: &[u8], trun_i: usize, trun_tam: usize) -> Option<u64> {
    let flags = u32_em(d, trun_i + CABECALHO)? & 0x00FF_FFFF;
    if flags & TRUN_SAMPLE_DURATION == 0 {
        // Sem durações por amostra não há nada a somar aqui; o tempo passa a vir do
        // `default_sample_duration` do `tfhd`, e nesse caso não se inventa.
        return Some(0);
    }
    let n = u32_em(d, trun_i + CABECALHO + 4)? as usize;
    let mut i = trun_i + CABECALHO + 8;
    if flags & TRUN_DATA_OFFSET != 0 {
        i += 4;
    }
    if flags & TRUN_FIRST_SAMPLE_FLAGS != 0 {
        i += 4;
    }
    let por_amostra = 4
        * ((flags & TRUN_SAMPLE_DURATION != 0) as usize
            + (flags & TRUN_SAMPLE_SIZE != 0) as usize
            + (flags & TRUN_SAMPLE_FLAGS != 0) as usize
            + (flags & TRUN_SAMPLE_CTS != 0) as usize);
    let mut total = 0u64;
    for k in 0..n {
        let j = i + k * por_amostra;
        if j + 4 > trun_i + trun_tam {
            return None;
        }
        total += u32_em(d, j)? as u64;
    }
    Some(total)
}

/// O `data_offset` contava a partir do endereço absoluto; passa a contar a partir do
/// início do `moof`, já com o tamanho que ele ficou a ter.
fn corrigir_trun(trun: &[u8], base: u64, posicao: u64, tamanho_tfdt: usize) -> Option<Vec<u8>> {
    let flags = u32_em(trun, CABECALHO)? & 0x00FF_FFFF;
    if flags & TRUN_DATA_OFFSET == 0 {
        return Some(trun.to_vec());
    }
    // [cabeçalho 8][versão+flags 4][sample_count 4][data_offset 4]
    let off_i = CABECALHO + 4 + 4;
    let antigo = u32_em(trun, off_i)? as i32 as i64;
    // O moof perdeu 8 bytes (o endereço absoluto) e ganhou os do tfdt.
    let delta = tamanho_tfdt as i64 - 8;
    let relativo = (base as i64 + antigo) - posicao as i64 + delta;
    if !(i32::MIN as i64..=i32::MAX as i64).contains(&relativo) {
        return None;
    }
    let mut v = trun.to_vec();
    v[off_i..off_i + 4].copy_from_slice(&(relativo as i32).to_be_bytes());
    Some(v)
}

#[cfg(test)]
mod testes {
    use super::*;

    fn caixa(tipo: &[u8; 4], corpo: &[u8]) -> Vec<u8> {
        let mut v = ((corpo.len() + CABECALHO) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(tipo);
        v.extend_from_slice(corpo);
        v
    }

    /// Um `moof` como o Media Foundation o escreve: `tfhd` com endereço absoluto, `trun`
    /// a contar a partir dele, e sem `tfdt` nenhum.
    fn moof_do_windows(base: u64, data_offset: i32) -> Vec<u8> {
        moof_do_windows_com_base(base, data_offset)
    }

    fn moof_do_windows_com_base(base: u64, data_offset: i32) -> Vec<u8> {
        let mut tfhd = Vec::new();
        tfhd.extend_from_slice(&(TFHD_BASE_DATA_OFFSET).to_be_bytes()); // versão 0 + flags
        tfhd.extend_from_slice(&1u32.to_be_bytes()); // track_ID
        tfhd.extend_from_slice(&base.to_be_bytes());
        let tfhd = caixa(b"tfhd", &tfhd);

        let mut trun = Vec::new();
        trun.extend_from_slice(&(TRUN_DATA_OFFSET | TRUN_SAMPLE_DURATION).to_be_bytes());
        trun.extend_from_slice(&3u32.to_be_bytes()); // sample_count
        trun.extend_from_slice(&data_offset.to_be_bytes());
        for _ in 0..3 {
            trun.extend_from_slice(&512u32.to_be_bytes()); // duração de cada amostra
        }
        let trun = caixa(b"trun", &trun);

        let mut traf = tfhd.clone();
        traf.extend_from_slice(&trun);
        let traf = caixa(b"traf", &traf);

        let mut corpo = caixa(b"mfhd", &[0, 0, 0, 0, 0, 0, 0, 7]);
        corpo.extend_from_slice(&traf);
        caixa(b"moof", &corpo)
    }

    #[test]
    fn traduz_o_fragmento_para_o_dialeto_do_mse() {
        // O moof comeca no byte 1000 e os dados dele estao em 1000 + 120.
        let original = moof_do_windows(1000, 120);
        let tam_antes = original.len();
        let (novo, duracao) = corrigir_moof(&original, 1000, 9000).expect("devia ter corrigido");

        // Perdeu o endereco absoluto (8) e ganhou o tfdt (20).
        assert_eq!(novo.len(), tam_antes - 8 + 20);
        assert_eq!(
            u32_em(&novo, 0).unwrap() as usize,
            novo.len(),
            "o moof tem de dizer o seu tamanho novo"
        );
        assert_eq!(duracao, 3 * 512, "a duracao e a soma das amostras");

        let (traf_i, traf_tam) = filha(&novo, CABECALHO, novo.len(), b"traf").unwrap();
        assert_eq!(
            traf_i + traf_tam,
            novo.len(),
            "o traf tem de fechar onde o moof fecha"
        );

        let (tfhd_i, _) = filha(&novo, traf_i + CABECALHO, traf_i + traf_tam, b"tfhd").unwrap();
        let flags = u32_em(&novo, tfhd_i + CABECALHO).unwrap() & 0x00FF_FFFF;
        assert_eq!(
            flags & TFHD_BASE_DATA_OFFSET,
            0,
            "o campo absoluto tem de sair"
        );
        assert_ne!(
            flags & TFHD_DEFAULT_BASE_IS_MOOF,
            0,
            "e a flag relativa entrar"
        );

        // O tfdt e o que faltava: sem ele o MSE recusa o fragmento sem dizer porque.
        let (tfdt_i, tfdt_tam) = filha(&novo, traf_i + CABECALHO, traf_i + traf_tam, b"tfdt")
            .expect("o tfdt tem de estar la");
        assert_eq!(tfdt_tam, 20);
        assert_eq!(
            u64_em(&novo, tfdt_i + CABECALHO + 4).unwrap(),
            9000,
            "e leva o tempo acumulado"
        );
        assert!(tfdt_i > tfhd_i, "e vem depois do tfhd");

        // 1000 + 120 - 1000 - 8 + 20 = 132: a mesma posicao, contada a partir do moof
        // com o tamanho que ele ficou a ter.
        let (trun_i, _) = filha(&novo, traf_i + CABECALHO, traf_i + traf_tam, b"trun").unwrap();
        let off = u32_em(&novo, trun_i + CABECALHO + 8).unwrap() as i32;
        assert_eq!(off, 132);
    }

    /// O caso a serio: se o `data_offset` traduzido nao apontar exatamente para os dados,
    /// o navegador descodifica lixo. Aqui monta-se o fluxo inteiro -- moof seguido do
    /// mdat -- e confirma-se que o ponteiro cai onde tem de cair.
    #[test]
    fn o_ponteiro_cai_mesmo_nos_dados() {
        let posicao = 4096u64;
        // O Media Foundation aponta para os dados do mdat, que ficam logo a seguir ao
        // moof: posicao + tamanho do moof + os 8 bytes do cabecalho do mdat.
        let tamanho_do_moof = moof_do_windows(0, 0).len() as u64;
        let base = posicao + tamanho_do_moof + 8;
        let moof = moof_do_windows_com_base(base, 0);

        let (novo, _) = corrigir_moof(&moof, posicao, 0).unwrap();
        let (traf_i, traf_tam) = filha(&novo, CABECALHO, novo.len(), b"traf").unwrap();
        let (trun_i, _) = filha(&novo, traf_i + CABECALHO, traf_i + traf_tam, b"trun").unwrap();
        let off = u32_em(&novo, trun_i + CABECALHO + 8).unwrap() as i32 as usize;

        // No fluxo de saida o mdat vem logo a seguir ao moof ja traduzido.
        assert_eq!(
            off,
            novo.len() + CABECALHO,
            "o data_offset tem de apontar para o primeiro byte de dados do mdat"
        );
    }

    #[test]
    fn le_o_codec_do_avcc_em_vez_de_o_assumir() {
        // avcC: versao, perfil 0x42 (Baseline), compat 0xC0, nivel 0x2A (4.2)
        let avcc = caixa(b"avcC", &[1, 0x42, 0xC0, 0x2A, 0xFF]);
        let mut avc1_corpo = vec![0u8; 78];
        avc1_corpo.extend_from_slice(&avcc);
        let avc1 = caixa(b"avc1", &avc1_corpo);
        // O stsd tem versao+flags e contagem antes das entradas: quem descer sem saltar
        // esses 8 bytes le lixo e nao encontra nada.
        let mut stsd_corpo = vec![0, 0, 0, 0, 0, 0, 0, 1];
        stsd_corpo.extend_from_slice(&avc1);
        let stsd = caixa(b"stsd", &stsd_corpo);
        let stbl = caixa(b"stbl", &stsd);
        let minf = caixa(b"minf", &stbl);
        let mdia = caixa(b"mdia", &minf);
        let trak = caixa(b"trak", &mdia);
        let moov = caixa(b"moov", &trak);

        assert_eq!(codec_do_moov(&moov).as_deref(), Some("avc1.42C02A"));
    }

    #[test]
    fn moov_sem_video_nao_inventa_codec() {
        let moov = caixa(b"moov", &caixa(b"mvhd", &[0; 100]));
        assert!(codec_do_moov(&moov).is_none());
    }

    #[test]
    fn nao_mexe_no_que_ja_esta_certo() {
        let mut tfhd = Vec::new();
        tfhd.extend_from_slice(&TFHD_DEFAULT_BASE_IS_MOOF.to_be_bytes());
        tfhd.extend_from_slice(&1u32.to_be_bytes());
        let traf = caixa(b"traf", &caixa(b"tfhd", &tfhd));
        let moof = caixa(b"moof", &traf);
        assert!(corrigir_moof(&moof, 0, 0).is_none());
    }

    #[test]
    fn caixa_estranha_passa_intacta_em_vez_de_ser_adivinhada() {
        // Um moof que diz ter um traf mas nao o tem inteiro.
        let moof = caixa(b"moof", &[0, 0, 0, 200, b't', b'r', b'a', b'f']);
        assert!(corrigir_moof(&moof, 0, 0).is_none());
    }

    #[test]
    fn caixa_incompleta_nao_e_dada_como_pronta() {
        let inteira = caixa(b"moof", &[1, 2, 3, 4]);
        assert_eq!(tamanho_da_caixa(&inteira, 0), Some(12));
        assert_eq!(tamanho_da_caixa(&inteira[..10], 0), None);
        // Um tamanho menor que o proprio cabecalho nao e uma caixa.
        assert_eq!(
            tamanho_da_caixa(&[0, 0, 0, 4, b'f', b't', b'y', b'p'], 0),
            None
        );
    }
}
