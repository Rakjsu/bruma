//! O som que sai das colunas, captado para seguir com a partilha de ecrã.
//!
//! # Porquê o `loopback` do WASAPI e não o navegador
//!
//! O caminho fácil seria `getDisplayMedia({audio: true})`, mas foi por causa dele que se
//! escreveu a captura nativa: traz a barra "está a partilhar" do WebView2, que não se
//! desliga. E o "Stereo Mix" — a outra saída fácil — vem desligado na maioria das placas e
//! obrigaria a pessoa a ir ao painel de som do Windows. Era exactamente a configuração
//! manual que este projecto passa a vida a apagar.
//!
//! O `loopback` do WASAPI não precisa de nada disso: pede-se ao dispositivo de SAÍDA que
//! entregue o que está a tocar. Sem drivers, sem definições, sem permissões.
//!
//! # O silêncio tem de ser fabricado
//!
//! Quando nada toca, o WASAPI em `loopback` não entrega pacote nenhum — não entrega
//! silêncio, entrega NADA. Se se deixasse assim, o contentor ficava com buracos e o som do
//! outro lado dessincronizava do vídeo assim que alguém fizesse uma pausa na música. Por
//! isso, quando o relógio avança e não veio nada, fabrica-se o silêncio que falta.

use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// O formato que o dispositivo nos deu. O WASAPI entrega no formato de mistura do
/// dispositivo e não naquele que nós pedimos — daí ser preciso ler o que ele diz.
#[derive(Clone, Copy, Debug)]
pub struct Formato {
    pub ritmo: u32,
    pub canais: u16,
    /// Bits por amostra por canal, já como o entregamos: 16. Fica escrito porque é a
    /// promessa que o `para_i16` cumpre e que o codificador lê.
    #[allow(dead_code)]
    pub bits: u16,
}

/// Um bocado de som, em PCM 16 bits intercalado, com o instante em que começou.
pub struct Bocado {
    pub pcm: Vec<u8>,
    /// Unidades de 100 ns desde o início da captura — a mesma unidade do vídeo.
    pub instante: i64,
    pub duracao: i64,
}

#[cfg(windows)]
mod win {
    use super::{Bocado, Formato, Result};
    use anyhow::anyhow;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use windows::Win32::Media::Audio::{
        eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator,
        MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
        AUDCLNT_STREAMFLAGS_LOOPBACK, WAVE_FORMAT_PCM,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED,
    };

    /// Converte o que o dispositivo entrega para PCM 16 bits, que é o que o codificador de
    /// AAC quer. O formato de mistura do Windows é quase sempre float de 32 bits.
    fn para_i16(bytes: &[u8], formato_float: bool, bits: u16) -> Vec<u8> {
        if formato_float && bits == 32 {
            let mut v = Vec::with_capacity(bytes.len() / 2);
            for c in bytes.chunks_exact(4) {
                let f = f32::from_le_bytes([c[0], c[1], c[2], c[3]]);
                // O clamp não é zelo a mais: uma mistura de várias apps passa de 1.0 com
                // facilidade, e sem isto dava a volta ao número — estalo em vez de saturar.
                let i = (f.clamp(-1.0, 1.0) * 32767.0) as i16;
                v.extend_from_slice(&i.to_le_bytes());
            }
            return v;
        }
        if bits == 16 {
            return bytes.to_vec();
        }
        // Formatos exóticos (24 bits, por exemplo) não se adivinham: silêncio do tamanho
        // certo é melhor do que ruído.
        vec![0u8; bytes.len() * 2 / (bits.max(8) as usize / 8)]
    }

    /// O formato do dispositivo de saída, sem começar a captar.
    ///
    /// Existe separado porque o codificador precisa de saber o ritmo e os canais para
    /// nascer, e só depois de ele nascer é que o som pode começar — de outra forma os dois
    /// relógios arrancavam em instantes diferentes e o som ficava adiantado.
    pub fn formato() -> Result<Formato> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let enumerador: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let dispositivo = enumerador.GetDefaultAudioEndpoint(eRender, eConsole)?;
            let cliente: IAudioClient = dispositivo.Activate(CLSCTX_ALL, None)?;
            let mix = cliente.GetMixFormat()?;
            let f = Formato {
                ritmo: (*mix).nSamplesPerSec,
                canais: (*mix).nChannels,
                bits: 16,
            };
            CoTaskMemFree(Some(mix as *const _));
            Ok(f)
        }
    }

    /// Capta o que está a tocar e chama `entrega` com cada bocado, até `parar`.
    ///
    /// Corre na thread que a chama — quem quiser noutra, que a lance.
    pub fn captar(
        parar: Arc<AtomicBool>,
        origem: std::time::Instant,
        anunciar: impl FnOnce(Formato),
        mut entrega: impl FnMut(Bocado),
    ) -> Result<()> {
        unsafe {
            // O `loopback` é COM puro; sem isto o CoCreateInstance falha na thread nova.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

            let enumerador: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            // eRender e não eCapture: em loopback pede-se ao dispositivo de SAÍDA o que
            // ele está a reproduzir. Pedir ao de captura dava o microfone.
            let dispositivo = enumerador.GetDefaultAudioEndpoint(eRender, eConsole)?;
            let cliente: IAudioClient = dispositivo.Activate(CLSCTX_ALL, None)?;

            let mix = cliente.GetMixFormat()?;
            let (ritmo, canais, bits) = {
                let f = &*mix;
                (f.nSamplesPerSec, f.nChannels, f.wBitsPerSample)
            };
            // O formato de mistura do Windows é float de 32 bits em quase todas as placas,
            // e vem marcado como EXTENSIBLE — daí o teste ser "não é PCM inteiro".
            let float = { (*mix).wFormatTag != WAVE_FORMAT_PCM as u16 && bits == 32 };

            // Buffer de 200 ms: folga que chegue para não perder som se a thread se
            // atrasar, sem acrescentar atraso perceptível a quem ouve.
            let resultado = cliente.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK,
                2_000_000,
                0,
                mix,
                None,
            );
            CoTaskMemFree(Some(mix as *const _));
            resultado?;

            let captura: IAudioCaptureClient = cliente.GetService()?;
            cliente.Start()?;

            anunciar(Formato {
                ritmo,
                canais,
                bits: 16,
            });

            let bytes_por_frame = canais as usize * 2; // já em i16
            let inicio = std::time::Instant::now();
            // O som não começa no instante zero da partilha: o codificador nasceu primeiro
            // e o dispositivo levou o seu tempo a abrir. Esse atraso mede-se UMA vez e
            // soma-se a tudo o que vier — é o que mantém o som colado à imagem em vez de
            // adiantado pelo tempo que a abertura demorou.
            let atraso = (origem.elapsed().as_nanos() / 100) as i64;
            // Onde vai o som já entregue, em frames. É este contador — e não o relógio da
            // parede — que decide quanto silêncio falta: assim o som nunca fica com mais
            // nem menos amostras do que o tempo que passou.
            let mut entregues: u64 = 0;

            while !parar.load(Ordering::Relaxed) {
                let mut pacote = captura.GetNextPacketSize().unwrap_or(0);
                if pacote == 0 {
                    // Nada a tocar. Vê-se quanto silêncio falta para o som acompanhar o
                    // relógio, e fabrica-se.
                    let decorrido = inicio.elapsed().as_secs_f64();
                    let devidos = (decorrido * ritmo as f64) as u64;
                    if devidos > entregues + ritmo as u64 / 50 {
                        let faltam = (devidos - entregues) as usize;
                        let instante = atraso + (entregues * 10_000_000 / ritmo as u64) as i64;
                        entrega(Bocado {
                            pcm: vec![0u8; faltam * bytes_por_frame],
                            instante,
                            duracao: (faltam as u64 * 10_000_000 / ritmo as u64) as i64,
                        });
                        entregues = devidos;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    continue;
                }

                while pacote > 0 {
                    let mut dados: *mut u8 = std::ptr::null_mut();
                    let mut frames = 0u32;
                    let mut flags = 0u32;
                    captura.GetBuffer(&mut dados, &mut frames, &mut flags, None, None)?;
                    if frames > 0 {
                        let mudo = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
                        let pcm = if mudo || dados.is_null() {
                            vec![0u8; frames as usize * bytes_por_frame]
                        } else {
                            let cru = std::slice::from_raw_parts(
                                dados,
                                frames as usize * canais as usize * (bits as usize / 8),
                            );
                            para_i16(cru, float, bits)
                        };
                        let instante = atraso + (entregues * 10_000_000 / ritmo as u64) as i64;
                        entregues += frames as u64;
                        entrega(Bocado {
                            pcm,
                            instante,
                            duracao: (frames as u64 * 10_000_000 / ritmo as u64) as i64,
                        });
                    }
                    captura.ReleaseBuffer(frames)?;
                    pacote = captura.GetNextPacketSize().unwrap_or(0);
                }
            }
            let _ = cliente.Stop();
            Ok(())
        }
    }

    /// Só para medir: capta durante `segundos` e diz o que ouviu.
    pub fn medir(segundos: u64) -> Result<()> {
        let parar = Arc::new(AtomicBool::new(false));
        let p2 = parar.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(segundos));
            p2.store(true, Ordering::Relaxed);
        });
        let mut bocados = 0u64;
        let mut amostras = 0u64;
        let mut pico = 0i32;
        let mut soma: f64 = 0.0;
        let mut formato = None;
        let inicio = std::time::Instant::now();
        captar(
            parar,
            inicio,
            |f| formato = Some(f),
            |b| {
                bocados += 1;
                for c in b.pcm.chunks_exact(2) {
                    let v = i16::from_le_bytes([c[0], c[1]]) as i32;
                    pico = pico.max(v.abs());
                    soma += (v as f64) * (v as f64);
                    amostras += 1;
                }
            },
        )?;
        let s = inicio.elapsed().as_secs_f64().max(0.001);
        let f = formato.ok_or_else(|| anyhow!("o dispositivo não disse o formato"))?;
        let rms = if amostras > 0 {
            (soma / amostras as f64).sqrt()
        } else {
            0.0
        };
        println!(
            "[som] {:.1}s: {} canais a {} Hz | {} bocados, {} amostras ({:.0}/s por canal, \
             esperadas {}) | pico {} rms {:.0}",
            s,
            f.canais,
            f.ritmo,
            bocados,
            amostras,
            amostras as f64 / s / f.canais as f64,
            f.ritmo,
            pico,
            rms,
        );
        Ok(())
    }
}

#[cfg(not(windows))]
mod win {
    use super::{Bocado, Formato, Result};
    use anyhow::anyhow;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    pub fn captar(
        _parar: Arc<AtomicBool>,
        _origem: std::time::Instant,
        _anunciar: impl FnOnce(Formato),
        _entrega: impl FnMut(Bocado),
    ) -> Result<()> {
        Err(anyhow!(
            "o som do sistema por enquanto só existe no Windows"
        ))
    }

    pub fn formato() -> Result<Formato> {
        Err(anyhow!(
            "o som do sistema por enquanto só existe no Windows"
        ))
    }

    pub fn medir(_s: u64) -> Result<()> {
        Err(anyhow!(
            "o som do sistema por enquanto só existe no Windows"
        ))
    }
}

pub use win::{captar, formato, medir};

/// Lança a captura numa thread própria, ancorada em `origem`.
pub fn arrancar(
    parar: Arc<AtomicBool>,
    origem: std::time::Instant,
    entrega: impl FnMut(Bocado) + Send + 'static,
) {
    std::thread::spawn(move || {
        if let Err(e) = captar(parar, origem, |_| {}, entrega) {
            eprintln!("[som] a captura do som terminou: {e:?}");
        }
    });
}

#[allow(dead_code)]
fn _usa(_: &Bocado, _: &Formato, _: &AtomicBool, _: &Ordering) {}
