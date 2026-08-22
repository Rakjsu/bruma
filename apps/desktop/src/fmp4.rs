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
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use windows::core::{implement, Ref, BOOL};
use windows::Win32::Media::MediaFoundation::*;

/// Cada pedaço leva à frente o que é.
///
/// Sem isto seria preciso um segundo caminho só para dizer o codec, e esse caminho teria
/// de existir duas vezes: uma para a pré-visualização local e outra para a rede. Assim há
/// um fluxo só, e ele descreve-se a si próprio.
pub const ETIQUETA_BYTES: u8 = 0;
pub const ETIQUETA_CODEC: u8 = 1;

/// Quem recebe os pedaços à medida que saem.
pub type Escoadouro = Arc<dyn Fn(&[u8]) + Send + Sync>;

struct Estado {
    dados: Vec<u8>,
    pos: u64,
    /// Até onde é que já foi entregue para fora.
    entregue: usize,
    /// Quantas caixas já foram anunciadas no diagnóstico.
    vistas: usize,
    /// Soma das durações dos fragmentos já entregues, para o `tfdt` de cada um.
    tempo: u64,
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
                pos: 0,
                entregue: 0,
                vistas: 0,
                tempo: 0,
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
        let inicio = e.pos as usize;
        let fim = inicio + bytes.len();
        if e.dados.len() < fim {
            e.dados.resize(fim, 0);
        }
        e.dados[inicio..fim].copy_from_slice(bytes);
        e.pos = fim as u64;

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
        let mut pendente: u64 = 0;
        while let Some(tam) = crate::mse::tamanho_da_caixa(&e.dados, e.entregue) {
            let i = e.entregue;
            e.entregue = i + tam;
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
            let traduzida = if crate::mse::e_moof(&e.dados, i) {
                crate::mse::corrigir_moof(caixa, i as u64, e.tempo + pendente)
            } else {
                None
            };
            match traduzida {
                Some((v, duracao)) => {
                    segmento.extend_from_slice(&v);
                    // O relógio só anda quando o segmento SAIR. Um `moof` cujo `mdat`
                    // ainda não chegou é reprocessado na escrita seguinte, e somar aqui
                    // contava-o duas e três vezes — os fragmentos iam parar a instantes
                    // cada vez mais tardios e o vídeo ficava cheio de buracos.
                    pendente += duracao;
                }
                None => segmento.extend_from_slice(caixa),
            }

            // O `moov` fecha o segmento de inicializacao e o `mdat` fecha um de media.
            let nome = crate::mse::nome_da_caixa(&e.dados, i);
            if nome == "moov" || nome == "mdat" {
                let mut com_etiqueta = Vec::with_capacity(1 + segmento.len());
                com_etiqueta.push(ETIQUETA_BYTES);
                com_etiqueta.append(&mut segmento);
                saida.push(com_etiqueta);
                e.tempo += pendente;
                pendente = 0;
                inicio_na_origem = e.entregue;
            }
        }
        // O que ficou a meio de um segmento espera pelo resto: nao se entrega um `moof`
        // sem o `mdat` dele.
        if !segmento.is_empty() {
            e.entregue = inicio_na_origem;
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
        Ok(self.estado.lock().unwrap().dados.len() as u64)
    }

    fn SetLength(&self, tamanho: u64) -> windows::core::Result<()> {
        self.estado
            .lock()
            .unwrap()
            .dados
            .resize(tamanho as usize, 0);
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
        Ok(BOOL::from(e.pos as usize >= e.dados.len()))
    }

    fn Read(&self, pb: *mut u8, cb: u32, lidos: *mut u32) -> windows::core::Result<()> {
        let mut e = self.estado.lock().unwrap();
        let inicio = (e.pos as usize).min(e.dados.len());
        let n = (cb as usize).min(e.dados.len() - inicio);
        // SAFETY: o chamador garante `cb` bytes em `pb`; copia-se no máximo isso.
        unsafe {
            std::ptr::copy_nonoverlapping(e.dados[inicio..].as_ptr(), pb, n);
            if !lidos.is_null() {
                *lidos = n as u32;
            }
        }
        e.pos = (inicio + n) as u64;
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
    /// 100 ns por frame, que é a unidade do Media Foundation.
    passo: i64,
    relogio: i64,
    /// Bytes de um frame de entrada, com as linhas coladas.
    tamanho_entrada: u32,
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
        para: Escoadouro,
    ) -> Result<Self> {
        arrancar_media_foundation()?;
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

            // MP4 fragmentado: cabeçalho uma vez, e a seguir fragmentos independentes.
            // É isto que faz a diferença entre um ficheiro e uma transmissão.
            let sink = MFCreateFMPEG4MediaSink(&byte_stream, None, &saida)?;

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
            escritor.SetInputMediaType(0, &entrada, None)?;

            escritor.BeginWriting()?;

            Ok(Self {
                escritor,
                fluxo: 0,
                passo: 10_000_000 / fps.max(1) as i64,
                relogio: 0,
                tamanho_entrada: largura * 4 * altura,
            })
        }
    }

    /// Um frame, em BGRA, com as linhas coladas (sem enchimento no fim de cada uma).
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
            amostra.SetSampleTime(self.relogio)?;
            amostra.SetSampleDuration(self.passo)?;
            self.relogio += self.passo;

            self.escritor.WriteSample(self.fluxo, &amostra)?;
            Ok(())
        }
    }

    pub fn terminar(self) -> Result<()> {
        unsafe { self.escritor.Finalize()? };
        Ok(())
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
    use std::sync::Once;
    static UMA_VEZ: Once = Once::new();
    static mut RESULTADO: Option<windows::core::Error> = None;

    UMA_VEZ.call_once(|| unsafe {
        if let Err(e) = MFStartup(MF_VERSION, MFSTARTUP_FULL) {
            RESULTADO = Some(e);
        }
    });
    // SAFETY: só se lê depois do `call_once`, que sincroniza a escrita.
    #[allow(static_mut_refs)]
    match unsafe { RESULTADO.as_ref() } {
        Some(e) => Err(anyhow!("o Media Foundation não arrancou: {e}")),
        None => Ok(()),
    }
}
