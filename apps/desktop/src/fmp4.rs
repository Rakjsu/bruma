//! Codificar o ecrã para MP4 fragmentado, em pedaços que saem à medida que são feitos.
//!
//! # Porque assim e não a martelar o codificador à mão
//!
//! O caminho "puro" seria falar diretamente com o MFT de H.264: pedir os `IMFSample` com
//! os NALs em Annex-B e mandá-los pela rede. Fica mais curto no fio e com menos latência,
//! mas obriga a fazer nós **duas** coisas que são exatamente onde se perde tempo e onde os
//! erros são silenciosos:
//!
//!  - **a conversão de cor.** A captura dá BGRA; os codificadores de hardware querem NV12.
//!    Fazer isso no CPU a 3440×1440 come mais do que a codificação inteira, e fazê-lo na
//!    GPU implica montar um `ID3D11VideoProcessor` à parte.
//!  - **o MFT da NVIDIA é assíncrono.** Não se chama `ProcessInput`/`ProcessOutput` e
//!    pronto: é preciso o `METransformNeedInput`/`METransformHaveOutput` e o desbloqueio
//!    com `MF_TRANSFORM_ASYNC_UNLOCK`, com todos os modos de falhar que isso traz.
//!
//! O `IMFSinkWriter` faz as duas por nós — escolhe o codificador (com hardware ligado por
//! `MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS`) e insere o conversor de cor sozinho.
//!
//! Falta-lhe uma coisa: o MP4 normal só fica legível no fim, porque o índice é escrito
//! depois de tudo. Isso não se transmite. A saída é por isso um **sink de MP4 fragmentado**
//! (`MFCreateFMPEG4MediaSink`), que escreve o cabeçalho uma vez e a seguir despeja
//! fragmentos independentes — cada um decifrável por si.
//!
//! # O byte stream é nosso de propósito
//!
//! O sink escreve para um `IMFByteStream`. Em vez de lhe dar um ficheiro, damos-lhe um
//! implementado aqui, que em vez de gravar chama uma função nossa com os bytes acabados de
//! escrever. É esse o cano por onde os fragmentos vão sair — para a rede, para a webview,
//! para onde for preciso — sem passarem pelo disco.
//!
//! Guarda-se na mesma o que já foi escrito, porque o sink recua para corrigir tamanhos de
//! caixas e precisa de poder ler o que lá pôs. O que se entrega para fora é só o que ainda
//! não tinha sido entregue: um `Seek` para trás não faz reenviar nada.

use anyhow::{anyhow, Result};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use windows::core::{implement, Interface, Ref, BOOL};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Variant::{VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_UI4};

/// Cada pedaço leva à frente o que é.
///
/// Sem isto seria preciso um segundo caminho só para dizer o codec, e esse caminho teria
/// de existir duas vezes: uma para a pré-visualização local e outra para a rede. Assim há
/// um fluxo só, e ele descreve-se a si próprio.
pub const ETIQUETA_BYTES: u8 = 0;
pub const ETIQUETA_CODEC: u8 = 1;

/// Quem recebe os pedaços à medida que saem.
pub type Escoadouro = Arc<dyn Fn(&[u8]) + Send + Sync>;

/// Medição (#111, #43): amostras de vídeo que SAÍRAM, quantas eram chave, e `moof` que
/// seguiram por traduzir. Estáticos porque quem os lê — a linha `[ecrã] fim` — vive na
/// thread do codificador e não tem o byte stream à mão. Contam-se à saída do segmento e
/// não à tradução: um `moof` cujo `mdat` ainda não chegou é traduzido outra vez na
/// escrita seguinte, e contá-lo aí dava o dobro.
pub static AMOSTRAS: AtomicU64 = AtomicU64::new(0);
pub static CHAVES: AtomicU64 = AtomicU64::new(0);
pub static MOOF_POR_TRADUZIR: AtomicU64 = AtomicU64::new(0);
/// Quando a chave a pedido foi pedida (#111), para medir quanto demorou a sair — e quanto
/// demorou a última.
pub static CHAVE_PEDIDA_EM: Mutex<Option<std::time::Instant>> = Mutex::new(None);
pub static CHAVE_DEMOROU_MS: AtomicU64 = AtomicU64::new(0);
/// O codificador que ficou a trabalhar (#110): o nome e se é de hardware. Para o registo
/// e para quem perguntar.
pub static CODIFICADOR: Mutex<String> = Mutex::new(String::new());

pub fn codificador_em_uso() -> String {
    CODIFICADOR.lock().map(|c| c.clone()).unwrap_or_default()
}

/// Quanto do passado se guarda. O sink recua para corrigir tamanhos de caixas, e não
/// pode recuar para além do que ainda temos — mas o recuo é sempre dentro do fragmento em
/// curso, que é da ordem das centenas de kilobytes. Quatro megabytes são folga de sobra.
const JANELA: u64 = 4 * 1024 * 1024;

struct Estado {
    dados: Vec<u8>,
    /// Posição absoluta do primeiro byte que ainda temos em `dados`.
    ///
    /// Sem isto o buffer crescia para sempre: uma hora de partilha a 7 Mbps são mais de
    /// três gigabytes guardados por nada, porque o que já saiu nunca mais é preciso.
    base: u64,
    pos: u64,
    /// Até onde é que já foi entregue para fora, em posição absoluta.
    entregue: u64,
    /// Quantas caixas já foram anunciadas no diagnóstico.
    vistas: usize,
    /// Soma das durações dos fragmentos já entregues, para o `tfdt` de cada um.
    /// Onde vai o relógio de cada faixa. Com som são duas, e cada uma tem o seu.
    tempo: crate::mse::Tempos,
}

#[implement(IMFByteStream)]
struct FluxoDeSaida {
    estado: Mutex<Estado>,
    para: Escoadouro,
    /// Quantos bytes a última escrita assíncrona levou. Ver `BeginWrite`.
    ultimos: AtomicU32,
}

impl FluxoDeSaida {
    fn novo(para: Escoadouro) -> Self {
        Self {
            estado: Mutex::new(Estado {
                dados: Vec::with_capacity(1 << 20),
                base: 0,
                pos: 0,
                entregue: 0,
                vistas: 0,
                tempo: Default::default(),
            }),
            para,
            ultimos: AtomicU32::new(0),
        }
    }

    /// Escreve na posição atual e entrega para fora as caixas que ficaram completas.
    ///
    /// A entrega é por caixas inteiras e não por bytes à medida que caem, por duas razões.
    /// A primeira é que o sink recua para corrigir tamanhos, e entregar um `moof` antes de
    /// ele estar fechado seria entregar um tamanho errado. A segunda é que os `moof` têm
    /// de ser traduzidos para o dialeto do MSE antes de saírem — e isso só se pode fazer
    /// com a caixa toda na mão.
    fn escrever(&self, bytes: &[u8]) -> u32 {
        let mut e = self.estado.lock().unwrap();
        // Se o sink recuar para antes do que ainda temos, não há nada de bom a fazer: os
        // bytes que ele quer corrigir já saíram. Descarta-se o remendo — mas conta-se como
        // escrito.
        //
        // Devolver 0 seria dizer ao Media Foundation "não escrevi nada", e ele traduz isso
        // por `E_UNEXPECTED` — foi o "Falha catastrófica (0x8000FFFF)" que aparecia a
        // 1440p ao PARAR de partilhar, com a transmissão inteira já entregue e boa (283
        // frames descodificados, sem erro nenhum do lado de quem via). O recuo é o
        // `Finalize` a voltar ao início para arrumar um ficheiro; numa transmissão ao vivo
        // esse arrumar não existe, porque os bytes já foram vistos. Aceitar e deitar fora
        // é o comportamento certo aqui, e não escreve nada no sítio errado.
        let Some(inicio) = e.pos.checked_sub(e.base).map(|v| v as usize) else {
            return bytes.len() as u32;
        };
        let fim = inicio + bytes.len();
        if e.dados.len() < fim {
            e.dados.resize(fim, 0);
        }
        e.dados[inicio..fim].copy_from_slice(bytes);
        e.pos = e.base + fim as u64;

        // O MSE come SEGMENTOS, nao caixas soltas: o de inicializacao e `ftyp`+`moov`
        // juntos, e cada segmento de media e um `moof`+`mdat` junto. Entregar as caixas
        // uma a uma obriga o parser a adivinhar onde acaba cada segmento, e e onde ele
        // desiste. De caminho poupa metade das travessias do IPC.
        let mut saida: Vec<Vec<u8>> = Vec::new();
        let mut segmento: Vec<u8> = Vec::new();
        // Onde o segmento a ser montado comecou NA ORIGEM. Nao serve medir pelo tamanho
        // do que ja se juntou: o `moof` traduzido e 8 bytes mais curto que o original, e
        // recuar por ele recuava a mais.
        let mut inicio_na_origem = e.entregue;
        let mut pendente = crate::mse::Tempos::default();
        let mut crus_pendentes = 0u64;
        while let Some(tam) = {
            let desde = (e.entregue - e.base) as usize;
            crate::mse::tamanho_da_caixa(&e.dados, desde)
        } {
            let i = (e.entregue - e.base) as usize;
            let absoluta = e.entregue;
            e.entregue += tam as u64;
            if crate::mse::VER_CAIXAS.load(std::sync::atomic::Ordering::Relaxed) && e.vistas < 14 {
                e.vistas += 1;
                println!("  [ecrã] caixa {}", crate::mse::nome_da_caixa(&e.dados, i));
            }
            if !crate::mse::interessa_ao_navegador(&e.dados, i) {
                continue;
            }
            let caixa = &e.dados[i..i + tam];
            // O `moov` diz qual e o codec, e quem recebe precisa de o saber ANTES de
            // abrir o buffer de video. Vai a frente dele, com etiqueta propria.
            if crate::mse::nome_da_caixa(&e.dados, i) == "moov" {
                if let Some(codec) = crate::mse::codec_do_moov(caixa) {
                    let mut aviso = vec![ETIQUETA_CODEC];
                    aviso.extend_from_slice(codec.as_bytes());
                    saida.push(aviso);
                }
            }
            let e_moof = crate::mse::e_moof(&e.dados, i);
            let traduzida = if e_moof && !crate::bandeiras::moof_cru() {
                crate::mse::corrigir_moof(caixa, absoluta, &e.tempo.mais(&pendente))
            } else {
                None
            };
            match traduzida {
                Some((v, duracoes)) => {
                    segmento.extend_from_slice(&v);
                    // O relógio só anda quando o segmento SAIR. Um `moof` cujo `mdat`
                    // ainda não chegou é reprocessado na escrita seguinte, e somar aqui
                    // contava-o duas e três vezes — os fragmentos iam parar a instantes
                    // cada vez mais tardios e o vídeo ficava cheio de buracos.
                    pendente.juntar(&duracoes);
                }
                None => {
                    // Um `moof` que segue cru sem já estar no dialecto certo é um
                    // fragmento que o MSE vai recusar (#43). Conta-se — à saída, como o
                    // resto — para a linha `[ecrã] fim` dizer quantos foram.
                    if e_moof && !crate::mse::moof_ja_no_dialecto(caixa) {
                        crus_pendentes += 1;
                    }
                    segmento.extend_from_slice(caixa)
                }
            }

            // O `moov` fecha o segmento de inicializacao e o `mdat` fecha um de media.
            let nome = crate::mse::nome_da_caixa(&e.dados, i);
            if nome == "moov" || nome == "mdat" {
                let mut com_etiqueta = Vec::with_capacity(1 + segmento.len());
                com_etiqueta.push(ETIQUETA_BYTES);
                com_etiqueta.append(&mut segmento);
                saida.push(com_etiqueta);
                e.tempo.juntar(&pendente);
                AMOSTRAS.fetch_add(pendente.amostras_de_video, Ordering::Relaxed);
                CHAVES.fetch_add(pendente.chaves_de_video, Ordering::Relaxed);
                MOOF_POR_TRADUZIR.fetch_add(crus_pendentes, Ordering::Relaxed);
                crus_pendentes = 0;
                if pendente.chaves_de_video > 0 {
                    // A chave que alguém pediu saiu neste segmento (#111): diz-se quanto
                    // demorou desde o pedido.
                    if let Some(em) = CHAVE_PEDIDA_EM.lock().unwrap().take() {
                        let ms = em.elapsed().as_millis() as u64;
                        CHAVE_DEMOROU_MS.store(ms, Ordering::Relaxed);
                        eprintln!(
                            "[ecrã] a chave pedida saiu ao fim de {ms} ms, no segmento com {} amostras ({} chaves), {} amostras saídas",
                            pendente.amostras_de_video, pendente.chaves_de_video, AMOSTRAS.load(Ordering::Relaxed)
                        );
                    }
                }
                pendente = Default::default();
                inicio_na_origem = e.entregue;
            }
        }
        // O que ficou a meio de um segmento espera pelo resto: nao se entrega um `moof`
        // sem o `mdat` dele.
        if !segmento.is_empty() {
            e.entregue = inicio_na_origem;
        }

        // E deita-se fora o passado que ja saiu, deixando a janela de folga para os
        // recuos do sink. Sem isto o buffer so cresce.
        let guardar_a_partir = e.entregue.saturating_sub(JANELA);
        if guardar_a_partir > e.base {
            let cortar = (guardar_a_partir - e.base) as usize;
            e.dados.drain(..cortar);
            e.base = guardar_a_partir;
        }
        drop(e);

        // Com --autoteste guarda-se tambem o que saiu, para se poder abrir a caixa e ver
        // os bytes em vez de discutir com o navegador aos palpites.
        if crate::mse::VER_CAIXAS.load(std::sync::atomic::Ordering::Relaxed) {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(std::env::temp_dir().join("bruma-mse.mp4"))
            {
                for p in &saida {
                    if p.first() == Some(&ETIQUETA_BYTES) {
                        let _ = f.write_all(&p[1..]);
                    }
                }
            }
        }
        for p in saida {
            (self.para)(&p);
        }
        bytes.len() as u32
    }
}

#[allow(non_snake_case)]
impl IMFByteStream_Impl for FluxoDeSaida_Impl {
    fn GetCapabilities(&self) -> windows::core::Result<u32> {
        // Lê, escreve e procura. Sem isto o sink recusa-se a usar o stream.
        Ok(MFBYTESTREAM_IS_READABLE | MFBYTESTREAM_IS_WRITABLE | MFBYTESTREAM_IS_SEEKABLE)
    }

    fn GetLength(&self) -> windows::core::Result<u64> {
        let e = self.estado.lock().unwrap();
        Ok(e.base + e.dados.len() as u64)
    }

    fn SetLength(&self, tamanho: u64) -> windows::core::Result<()> {
        let mut e = self.estado.lock().unwrap();
        let novo = tamanho.saturating_sub(e.base) as usize;
        e.dados.resize(novo, 0);
        Ok(())
    }

    fn GetCurrentPosition(&self) -> windows::core::Result<u64> {
        Ok(self.estado.lock().unwrap().pos)
    }

    fn SetCurrentPosition(&self, pos: u64) -> windows::core::Result<()> {
        self.estado.lock().unwrap().pos = pos;
        Ok(())
    }

    fn IsEndOfStream(&self) -> windows::core::Result<BOOL> {
        let e = self.estado.lock().unwrap();
        Ok(BOOL::from(e.pos >= e.base + e.dados.len() as u64))
    }

    fn Read(&self, pb: *mut u8, cb: u32, lidos: *mut u32) -> windows::core::Result<()> {
        let mut e = self.estado.lock().unwrap();
        let inicio = e.pos.saturating_sub(e.base).min(e.dados.len() as u64) as usize;
        let n = (cb as usize).min(e.dados.len() - inicio);
        // SAFETY: o chamador garante `cb` bytes em `pb`; copia-se no máximo isso.
        unsafe {
            std::ptr::copy_nonoverlapping(e.dados[inicio..].as_ptr(), pb, n);
            if !lidos.is_null() {
                *lidos = n as u32;
            }
        }
        e.pos = e.base + (inicio + n) as u64;
        Ok(())
    }

    /// Pelo mesmo motivo do `BeginWrite`: não devolver `E_NOTIMPL` a quem espera.
    fn BeginRead(
        &self,
        pb: *mut u8,
        cb: u32,
        callback: Ref<IMFAsyncCallback>,
        estado: Ref<windows::core::IUnknown>,
    ) -> windows::core::Result<()> {
        let mut lidos: u32 = 0;
        self.Read(pb, cb, &mut lidos)?;
        self.ultimos.store(lidos, Ordering::SeqCst);
        unsafe {
            let resultado = MFCreateAsyncResult(None, callback.as_ref(), estado.as_ref())?;
            MFInvokeCallback(&resultado)?;
        }
        Ok(())
    }

    fn EndRead(&self, _r: Ref<IMFAsyncResult>) -> windows::core::Result<u32> {
        Ok(self.ultimos.load(Ordering::SeqCst))
    }

    fn Write(&self, pb: *const u8, cb: u32) -> windows::core::Result<u32> {
        // SAFETY: o chamador garante `cb` bytes legíveis em `pb`.
        let bytes = unsafe { std::slice::from_raw_parts(pb, cb as usize) };
        Ok(self.escrever(bytes))
    }

    /// O caminho assíncrono, e **não** é opcional.
    ///
    /// Custou uma tarde a perceber: com estes dois a devolver `E_NOTIMPL`, o sink de MP4
    /// fragmentado não dá erro nenhum — fica simplesmente à espera para sempre de uma
    /// conclusão que nunca chega. Bloqueia sem uma linha de aviso, e do lado de fora
    /// parece que a captura é lenta.
    ///
    /// Escreve-se logo (a escrita é para memória, não vale a pena adiar) e avisa-se pela
    /// fila de trabalho do Media Foundation. Chamar o `Invoke` aqui diretamente também
    /// funcionaria, mas reentraria no sink a meio de ele estar a escrever.
    fn BeginWrite(
        &self,
        pb: *const u8,
        cb: u32,
        callback: Ref<IMFAsyncCallback>,
        estado: Ref<windows::core::IUnknown>,
    ) -> windows::core::Result<()> {
        // SAFETY: o chamador garante `cb` bytes legíveis em `pb`.
        let bytes = unsafe { std::slice::from_raw_parts(pb, cb as usize) };
        let escritos = self.escrever(bytes);
        self.ultimos.store(escritos, Ordering::SeqCst);

        unsafe {
            let resultado = MFCreateAsyncResult(None, callback.as_ref(), estado.as_ref())?;
            MFInvokeCallback(&resultado)?;
        }
        Ok(())
    }

    fn EndWrite(&self, _r: Ref<IMFAsyncResult>) -> windows::core::Result<u32> {
        // O sink escreve uma de cada vez, portanto o último contador é o desta. Se algum
        // dia houver várias escritas em voo, isto passa a ter de viajar no próprio
        // resultado em vez de num contador partilhado.
        Ok(self.ultimos.load(Ordering::SeqCst))
    }

    fn Seek(
        &self,
        origem: MFBYTESTREAM_SEEK_ORIGIN,
        desvio: i64,
        _flags: u32,
    ) -> windows::core::Result<u64> {
        let mut e = self.estado.lock().unwrap();
        let base = if origem == msoBegin {
            0i64
        } else {
            e.pos as i64
        };
        e.pos = (base + desvio).max(0) as u64;
        Ok(e.pos)
    }

    fn Flush(&self) -> windows::core::Result<()> {
        Ok(())
    }

    fn Close(&self) -> windows::core::Result<()> {
        Ok(())
    }
}

/// O codificador: recebe texturas BGRA e cospe fragmentos de MP4.
pub struct Codificador {
    escritor: IMFSinkWriter,
    fluxo: u32,
    /// Quando a partilha começou. Os instantes das amostras saem DAQUI e não de um
    /// contador de frames — ver `frame`. Vem de fora porque o som usa o MESMO relógio: se
    /// cada um tivesse o seu, a imagem e o som ficavam separados pelo tempo que a abertura
    /// do dispositivo demorasse.
    inicio: std::time::Instant,
    /// Fim da última amostra, em unidades de 100 ns desde `inicio`.
    ultimo: i64,
    /// Bytes de um frame de entrada, com as linhas coladas.
    tamanho_entrada: u32,
    /// O fluxo do som, quando existe. `None` quando a partilha vai muda.
    fluxo_som: Option<u32>,
    /// Bytes de PCM que o som entregou, e o ritmo com que a faixa foi DECLARADA (#108).
    /// A divisão de um pelo outro é o tempo que a faixa julga ter — e tem de bater com o
    /// tempo de parede. Não bate quando a sondagem que declarou o ritmo se enganou.
    bytes_som: u64,
    ritmo_declarado: u32,
    /// A porta para pedir um frame completo ao codificador (#111). `None` quando ele não
    /// a dá — ou quando `BRUMA_SEM_CHAVE_A_PEDIDO` a fecha, para se medir o de antes.
    chave: Option<ICodecAPI>,
}

impl Codificador {
    /// Entra a `largura`×`altura` da captura e sai a `lar_saida`×`alt_saida`. As duas
    /// podem ser diferentes: o `IMFSinkWriter` também trata da redução, e é assim que um
    /// ecrã ultrawide cabe no upload de alguém sem termos de escalar nós.
    ///
    /// `para` é chamada com cada pedaço de MP4 assim que ele existe.
    #[allow(clippy::too_many_arguments)]
    pub fn novo(
        largura: u32,
        altura: u32,
        lar_saida: u32,
        alt_saida: u32,
        fps: u32,
        bitrate: u32,
        som: Option<crate::som::Formato>,
        origem: std::time::Instant,
        para: Escoadouro,
    ) -> Result<Self> {
        arrancar_media_foundation()?;
        // Os contadores da medição são de UMA partilha: recomeçam com o codificador.
        AMOSTRAS.store(0, Ordering::Relaxed);
        CHAVES.store(0, Ordering::Relaxed);
        MOOF_POR_TRADUZIR.store(0, Ordering::Relaxed);
        CHAVE_DEMOROU_MS.store(0, Ordering::Relaxed);
        *CHAVE_PEDIDA_EM.lock().unwrap() = None;
        if crate::mse::VER_CAIXAS.load(std::sync::atomic::Ordering::Relaxed) {
            let _ = std::fs::remove_file(std::env::temp_dir().join("bruma-mse.mp4"));
        }
        unsafe {
            let byte_stream: IMFByteStream = FluxoDeSaida::novo(para).into();

            // O tipo de SAÍDA descreve o que queremos que saia: H.264 com este tamanho,
            // este ritmo e este débito.
            let saida: IMFMediaType = MFCreateMediaType()?;
            saida.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            saida.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            saida.SetUINT32(&MF_MT_AVG_BITRATE, bitrate)?;
            saida.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            juntar64(&saida, &MF_MT_FRAME_SIZE, lar_saida, alt_saida)?;
            juntar64(&saida, &MF_MT_FRAME_RATE, fps, 1)?;
            juntar64(&saida, &MF_MT_PIXEL_ASPECT_RATIO, 1, 1)?;
            // Um frame COMPLETO pelo menos a cada `2×fps` amostras — em teoria. Medido com o
            // codificador da NVIDIA: 20 chaves em 586 amostras, uma a cada 30, com isto a
            // pedir 60. Ele ignora o pedido e fica no seu GOP de 30; fica-se a pedir na
            // mesma, para os codificadores que o honram. E «amostras» não são frames
            // capturados: o Media Foundation repete o último frame até ao ritmo declarado
            // (28 frames capturados em 20 s deram 600 amostras), por isso 30 amostras são
            // um segundo de parede a 30 ips, esteja o ecrã parado ou não.
            //
            // # Porque é que isto tem de ser dito
            //
            // Os frames normais só descrevem o que mudou desde o anterior. Quem carrega em
            // "Assistir" a meio recebe o cabeçalho guardado — mas o cabeçalho é o `ftyp` e
            // o `moov`, que dizem QUE codec é, não o que está no ecrã. Até chegar um frame
            // completo, o que ele tem são diferenças em relação a imagens que nunca viu —
            // e o MSE deita-as fora: «à espera da imagem…».
            //
            // O espaçamento é o TECTO, para o caso de ninguém pedir nada. O que faz quem
            // entra a meio ver a imagem no frame seguinte é a chave a pedido (#111): ver
            // `forcar_chave`, chamada pelo laço do codificador quando entra um espectador.
            saida.SetUINT32(&MF_MT_MAX_KEYFRAME_SPACING, fps.max(1) * 2)?;

            // MP4 fragmentado: cabeçalho uma vez, e a seguir fragmentos independentes.
            // É isto que faz a diferença entre um ficheiro e uma transmissão.
            // A faixa de som entra AQUI, na criação do sink — não se acrescenta depois.
            // O AAC é a escolha certa por ser o que o contentor MP4 e o MSE já falam: não
            // acrescenta codificador nenhum ao projecto nem um segundo caminho na rede.
            let som_saida: Option<IMFMediaType> = match som {
                Some(f) => {
                    let t: IMFMediaType = MFCreateMediaType()?;
                    t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
                    t.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)?;
                    t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, ritmo_aac(f.ritmo))?;
                    t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, f.canais.min(2) as u32)?;
                    t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
                    t.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 16_000)?;
                    // AAC cru, sem o cabeçalho ADTS: dentro de um MP4 o cabeçalho está no
                    // `esds` e repeti-lo em cada bloco seria lixo que alguns leitores
                    // recusam. E o nível 2 é o perfil que cobre estéreo a 48 kHz.
                    t.SetUINT32(&MF_MT_AAC_PAYLOAD_TYPE, 0)?;
                    t.SetUINT32(&MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION, 0x29)?;
                    Some(t)
                }
                None => None,
            };
            // A ordem é (fluxo, VÍDEO, ÁUDIO). Vale a pena dizer porquê: até aqui o código
            // passava o vídeo no lugar do áudio e funcionava à mesma — o sink não valida os
            // lugares, cria um fluxo por cada tipo que lhe dão. Com uma faixa só isso nunca
            // se notou; com duas, dava `MF_E_TOPO_CODEC_NOT_FOUND`, porque ficava a
            // procurar um transformador de BGRA para AAC.
            let sink = MFCreateFMPEG4MediaSink(&byte_stream, &saida, som_saida.as_ref())?;

            // E qual dos fluxos é qual? PERGUNTA-SE, em vez de se assumir que o vídeo é o
            // zero. Assumir era o mesmo erro outra vez, só que mais tarde e mais difícil de
            // ver: com os índices trocados o vídeo iria pelo caminho do som, em silêncio.
            let mut fluxo_video = 0u32;
            let mut indice_som = None;
            for i in 0..sink.GetStreamSinkCount().unwrap_or(0) {
                let Ok(fluxo) = sink.GetStreamSinkByIndex(i) else {
                    continue;
                };
                let Ok(tipo) = fluxo.GetMediaTypeHandler().and_then(|h| h.GetMajorType()) else {
                    continue;
                };
                if tipo == MFMediaType_Video {
                    fluxo_video = i;
                } else if tipo == MFMediaType_Audio {
                    indice_som = Some(i);
                }
            }

            let atributos: IMFAttributes = {
                let mut a = None;
                MFCreateAttributes(&mut a, 4)?;
                a.ok_or_else(|| anyhow!("sem atributos"))?
            };
            // Sem isto, o Media Foundation fica-se pelo codificador de software.
            atributos.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
            // Diz ao codificador para não juntar frames à espera de comprimir melhor.
            atributos.SetUINT32(&MF_LOW_LATENCY, 1)?;

            let escritor = MFCreateSinkWriterFromMediaSink(&sink, &atributos)?;

            // O tipo de ENTRADA é o que lhe vamos dar: BGRA da captura. O sink writer
            // insere sozinho o conversor para NV12 — que é a razão principal de estarmos
            // a passar por ele em vez de falar com o MFT à mão.
            let entrada: IMFMediaType = MFCreateMediaType()?;
            entrada.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            entrada.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_ARGB32)?;
            entrada.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            juntar64(&entrada, &MF_MT_FRAME_SIZE, largura, altura)?;
            // A captura entrega as linhas de cima para baixo. Em ARGB32 o Media Foundation
            // assume o contrário — sem esta linha a imagem sai ao contrário, e é o tipo de
            // erro que só se vê quando já há alguém a olhar para ela.
            entrada.SetUINT32(&MF_MT_DEFAULT_STRIDE, largura * 4)?;
            juntar64(&entrada, &MF_MT_FRAME_RATE, fps, 1)?;
            juntar64(&entrada, &MF_MT_PIXEL_ASPECT_RATIO, 1, 1)?;
            escritor.SetInputMediaType(fluxo_video, &entrada, None)?;

            // O som entra em PCM de 16 bits; o sink writer põe o codificador de AAC pelo
            // meio sozinho — e, se o dispositivo der um ritmo que o AAC não fala, põe
            // também o reamostrador. É a mesma razão por que o vídeo entra em BGRA.
            let fluxo_som = match som.zip(indice_som) {
                Some((f, indice)) => {
                    let t: IMFMediaType = MFCreateMediaType()?;
                    let canais = f.canais.min(2) as u32;
                    t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
                    t.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
                    t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, f.ritmo)?;
                    t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, canais)?;
                    t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
                    t.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, canais * 2)?;
                    t.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, f.ritmo * canais * 2)?;
                    match escritor.SetInputMediaType(indice, &t, None) {
                        Ok(()) => Some(indice),
                        Err(e) => {
                            eprintln!("[som] o som não entra no contentor: {e:?}");
                            None
                        }
                    }
                }
                None => None,
            };

            escritor.BeginWriting()?;

            // O CODIFICADOR QUE FICOU A TRABALHAR (#110). O pedido de transformadores de
            // hardware lá em cima é um pedido, não uma garantia: sem placa, ou com um
            // driver que não fala H.264, o sink writer põe o codificador de software e não
            // diz nada. A diferença é um núcleo inteiro por partilha, e ninguém a via.
            let (nome, hardware) = quem_codifica(&escritor, fluxo_video);
            let descricao = format!(
                "{nome} ({})",
                match hardware {
                    Some(true) => "hardware",
                    Some(false) => "software",
                    None => "não se sabe se é hardware",
                }
            );
            eprintln!("[ecrã] codificador: {descricao}");
            *CODIFICADOR.lock().unwrap() = descricao;
            // A chave a pedido (#111): quem entra a meio recebe um frame completo no
            // frame seguinte, e não quando o espaçamento calhar.
            let chave = if crate::bandeiras::sem_chave_a_pedido() {
                eprintln!("[ecrã] chave a pedido desligada, a pedido (BRUMA_SEM_CHAVE_A_PEDIDO)");
                None
            } else {
                codec_api(&escritor, fluxo_video)
            };

            Ok(Self {
                escritor,
                fluxo: fluxo_video,
                inicio: origem,
                ultimo: 0,
                fluxo_som,
                tamanho_entrada: largura * 4 * altura,
                bytes_som: 0,
                ritmo_declarado: som.map(|f| f.ritmo).unwrap_or(0),
                chave,
            })
        }
    }

    /// Um frame, em BGRA, com as linhas coladas (sem enchimento no fim de cada uma).
    ///
    /// # O relógio vem da PAREDE, não da contagem de frames
    ///
    /// Antes, cada amostra durava `1/fps` e o relógio andava um passo por frame. Parece
    /// certo e não é: a captura do Windows só entrega frames quando o ecrã MUDA, por isso
    /// o número de frames por segundo não é o que se pediu — é o que aconteceu. Medido num
    /// ecrã sossegado: pedidos 15 ips, entregues 2,1/s; o relógio da média chegava a 1,1 s
    /// enquanto a parede ia em 8,3 s. Quem estava a ver recebia câmara lenta e ficava cada
    /// vez mais atrasado, para sempre.
    ///
    /// Agora o instante e a duração de cada amostra saem do relógio real. A linha temporal
    /// passa a bater com o tempo, entregue o Windows os frames ao ritmo que entregar — e é
    /// por isso que esta correcção vale mais do que travar o ritmo: torna o sistema imune
    /// ao ritmo, em vez de depender de ele ser o esperado.
    pub fn frame(&mut self, bgra: &[u8]) -> Result<()> {
        unsafe {
            let esperado = self.tamanho_entrada;
            let buffer: IMFMediaBuffer = MFCreateMemoryBuffer(esperado)?;
            let mut destino: *mut u8 = std::ptr::null_mut();
            buffer.Lock(&mut destino, None, None)?;
            let n = (esperado as usize).min(bgra.len());
            std::ptr::copy_nonoverlapping(bgra.as_ptr(), destino, n);
            buffer.Unlock()?;
            buffer.SetCurrentLength(esperado)?;

            let amostra: IMFSample = MFCreateSample()?;
            amostra.AddBuffer(&buffer)?;
            // Este frame ocupa desde o fim do anterior até agora. A soma das durações
            // fica igual ao tempo real decorrido, que é exactamente o que o `tfdt` do MSE
            // lê para situar cada fragmento no tempo.
            let agora = (self.inicio.elapsed().as_nanos() / 100) as i64;
            let duracao = (agora - self.ultimo).max(1);
            amostra.SetSampleTime(self.ultimo)?;
            amostra.SetSampleDuration(duracao)?;
            self.ultimo = agora;

            self.escritor.WriteSample(self.fluxo, &amostra)?;
            Ok(())
        }
    }

    /// Um bocado de som, em PCM de 16 bits intercalado. Silencioso se a partilha for muda.
    pub fn som(&mut self, pcm: &[u8], instante: i64, duracao: i64) -> Result<()> {
        let Some(fluxo) = self.fluxo_som else {
            return Ok(());
        };
        if pcm.is_empty() {
            return Ok(());
        }
        self.bytes_som += pcm.len() as u64;
        unsafe {
            let buffer: IMFMediaBuffer = MFCreateMemoryBuffer(pcm.len() as u32)?;
            let mut destino: *mut u8 = std::ptr::null_mut();
            buffer.Lock(&mut destino, None, None)?;
            std::ptr::copy_nonoverlapping(pcm.as_ptr(), destino, pcm.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(pcm.len() as u32)?;

            let amostra: IMFSample = MFCreateSample()?;
            amostra.AddBuffer(&buffer)?;
            amostra.SetSampleTime(instante)?;
            amostra.SetSampleDuration(duracao.max(1))?;
            self.escritor.WriteSample(fluxo, &amostra)?;
        }
        Ok(())
    }

    /// Onde vai a linha temporal da média, em segundos. Serve para a medição poder
    /// comparar com o relógio da parede: os dois têm de andar juntos.
    pub fn relogio_media(&self) -> f64 {
        self.ultimo as f64 / 10_000_000.0
    }

    /// Quanto tempo a faixa de som JULGA ter, pelo ritmo com que foi declarada (#108):
    /// bytes de PCM a dividir por (ritmo × 2 canais × 2 bytes). Zero se a partilha é muda.
    /// A comparar com o tempo de parede: x1,00 é o certo; x1,09 é uma faixa declarada a
    /// 44,1 kHz e alimentada a 48.
    pub fn relogio_som_declarado(&self) -> f64 {
        if self.ritmo_declarado == 0 {
            return 0.0;
        }
        self.bytes_som as f64 / (self.ritmo_declarado as f64 * 4.0)
    }

    /// Pede um frame completo no PRÓXIMO frame (#111). `true` se o codificador aceitou.
    ///
    /// # O que isto compra, medido
    ///
    /// Menos do que se esperava. A chave natural vem a cada 30 amostras — o codificador da
    /// NVIDIA ignora o `MF_MT_MAX_KEYFRAME_SPACING` e fica nos 30 — e, como o Media
    /// Foundation enche o fluxo até ao ritmo declarado, isso é um segundo de parede a
    /// 30 ips, faça o ecrã o que fizer. A chave forçada aparece no fluxo 2 a 7 amostras
    /// depois do pedido (visto no `bruma-mse.mp4`, fora da grelha das naturais), mas o
    /// tempo desde a entrada do espectador até lhe sair um segmento com chave não desceu:
    /// 585-966 ms com ela, 26-789 ms sem ela, cinco entradas cada. O atraso do pipeline
    /// manda. Fica porque é barata e porque há codificadores que honram GOPs longos; ver o
    /// laço em `ecra.rs` para a outra metade e para os números.
    pub fn forcar_chave(&self) -> bool {
        let Some(api) = &self.chave else {
            return false;
        };
        unsafe {
            // Um VARIANT de `VT_UI4` a 1, montado à mão como o PROPVARIANT do som: não há
            // construtor para isto no crate.
            let um = std::mem::ManuallyDrop::new(VARIANT {
                Anonymous: VARIANT_0 {
                    Anonymous: std::mem::ManuallyDrop::new(VARIANT_0_0 {
                        vt: VT_UI4,
                        wReserved1: 0,
                        wReserved2: 0,
                        wReserved3: 0,
                        Anonymous: VARIANT_0_0_0 { ulVal: 1 },
                    }),
                },
            });
            match api.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &*um) {
                Ok(()) => true,
                Err(e) => {
                    eprintln!("[ecrã] o codificador não aceitou a chave a pedido: {e:?}");
                    false
                }
            }
        }
    }

    pub fn terminar(self) -> Result<()> {
        unsafe { self.escritor.Finalize()? };
        Ok(())
    }
}

/// Quem está mesmo a codificar (#110): percorre a cadeia de transformadores que o sink
/// writer montou para o fluxo de vídeo e procura o da categoria «codificador de vídeo». O
/// nome e a marca de hardware vêm dos atributos do próprio transformador — um MFT de
/// hardware é obrigado a expor `MFT_ENUM_HARDWARE_URL_Attribute` lá.
unsafe fn quem_codifica(escritor: &IMFSinkWriter, fluxo: u32) -> (String, Option<bool>) {
    let Ok(ex) = escritor.cast::<IMFSinkWriterEx>() else {
        return ("sem IMFSinkWriterEx".into(), None);
    };
    for i in 0..8u32 {
        let mut categoria = windows::core::GUID::zeroed();
        let mut transformador: Option<IMFTransform> = None;
        if ex
            .GetTransformForStream(fluxo, i, Some(&mut categoria), &mut transformador)
            .is_err()
        {
            break;
        }
        let Some(t) = transformador else {
            break;
        };
        if categoria != MFT_CATEGORY_VIDEO_ENCODER {
            continue;
        }
        let Ok(atributos) = t.GetAttributes() else {
            return ("codificador sem atributos".into(), Some(false));
        };
        let nome = texto_do_atributo(&atributos, &MFT_FRIENDLY_NAME_Attribute)
            .unwrap_or_else(|| "codificador sem nome".into());
        let hardware = texto_do_atributo(&atributos, &MFT_ENUM_HARDWARE_URL_Attribute).is_some()
            || atributos.GetUINT32(&MF_TRANSFORM_ASYNC).unwrap_or(0) == 1;
        return (nome, Some(hardware));
    }
    ("codificador não encontrado na cadeia".into(), None)
}

unsafe fn texto_do_atributo(a: &IMFAttributes, chave: &windows::core::GUID) -> Option<String> {
    let mut p = windows::core::PWSTR::null();
    let mut n = 0u32;
    a.GetAllocatedString(chave, &mut p, &mut n).ok()?;
    let texto = p.to_string().ok();
    windows::Win32::System::Com::CoTaskMemFree(Some(p.as_ptr() as *const _));
    texto
}

/// A porta do codificador para pedidos fora do tipo de média (#111). `GUID_NULL` como
/// serviço é a forma documentada de chegar ao `ICodecAPI` do codificador de um fluxo.
unsafe fn codec_api(escritor: &IMFSinkWriter, fluxo: u32) -> Option<ICodecAPI> {
    let mut p: *mut std::ffi::c_void = std::ptr::null_mut();
    match escritor.GetServiceForStream(
        fluxo,
        &windows::core::GUID::zeroed(),
        &ICodecAPI::IID,
        &mut p,
    ) {
        Ok(()) if !p.is_null() => Some(ICodecAPI::from_raw(p)),
        Ok(()) => None,
        Err(e) => {
            eprintln!("[ecrã] o codificador não dá ICodecAPI; sem chave a pedido: {e:?}");
            None
        }
    }
}

/// O codificador de AAC do Windows só fala 44100 e 48000. Para o resto, escolhe-se o mais
/// perto e deixa-se o sink writer pôr o reamostrador — em vez de recusar o som todo.
fn ritmo_aac(ritmo: u32) -> u32 {
    if ritmo == 44_100 {
        44_100
    } else {
        48_000
    }
}

/// Vários atributos do Media Foundation são um par de u32 metidos num u64 — o tamanho, o
/// ritmo, o rácio. Escrever isto à mão em cada sítio é onde se troca a largura pela altura.
fn juntar64(t: &IMFMediaType, chave: &windows::core::GUID, alto: u32, baixo: u32) -> Result<()> {
    unsafe { t.SetUINT64(chave, ((alto as u64) << 32) | baixo as u64)? };
    Ok(())
}

/// O Media Foundation tem de ser arrancado uma vez por processo antes de se lhe pedir
/// seja o que for.
///
/// No spike isto acontecia por acidente — a listagem de codificadores chamava o
/// `MFStartup` e o resto vinha atrás. Aqui não havia quem o chamasse, e o sintoma não
/// ajudava nada: o `MFCreateFMPEG4MediaSink` respondia `MF_E_SHUTDOWN`, "Shutdown() foi
/// chamado", quando o problema era exatamente o contrário — nunca tinha sido arrancado.
fn arrancar_media_foundation() -> Result<()> {
    static RESULTADO: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    let erro = RESULTADO.get_or_init(|| unsafe {
        MFStartup(MF_VERSION, MFSTARTUP_FULL)
            .err()
            .map(|e| e.to_string())
    });
    match erro {
        Some(e) => Err(anyhow!("o Media Foundation não arrancou: {e}")),
        None => Ok(()),
    }
}
