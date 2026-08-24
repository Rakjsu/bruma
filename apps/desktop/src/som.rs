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
        AUDCLNT_STREAMFLAGS_LOOPBACK, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
    };
    // Estas três vivem noutros módulos do mesmo SDK, e é por isso que estão à parte.
    use windows::Win32::Media::KernelStreaming::WAVE_FORMAT_EXTENSIBLE;
    use windows::Win32::Media::Multimedia::{
        KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED,
    };

    /// Uma amostra do dispositivo, seja qual for o formato dele, como um número entre
    /// -1 e 1. Devolve `None` para formatos que não se sabem ler — e nesse caso prefere-se
    /// silêncio a adivinhar, porque adivinhar aqui soa a ruído branco a todo o volume.
    fn amostra(bytes: &[u8], i: usize, forma: Forma) -> Option<f32> {
        match forma {
            Forma::Float32 => {
                let c = bytes.get(i * 4..i * 4 + 4)?;
                Some(f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            }
            Forma::Int16 => {
                let c = bytes.get(i * 2..i * 2 + 2)?;
                Some(i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
            }
            Forma::Int32 => {
                let c = bytes.get(i * 4..i * 4 + 4)?;
                Some(i32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f32 / 2_147_483_648.0)
            }
            Forma::Int24 => {
                let c = bytes.get(i * 3..i * 3 + 3)?;
                // 24 bits com sinal, em três bytes little-endian: sobe-se para 32 bits e
                // desce-se outra vez, que é a forma barata de estender o sinal.
                let v = ((c[0] as i32) << 8) | ((c[1] as i32) << 16) | ((c[2] as i32) << 24);
                Some(v as f32 / 2_147_483_648.0)
            }
            Forma::Desconhecida => None,
        }
    }

    /// Converte o que o dispositivo entrega para PCM de 16 bits **em estéreo**, que é o que
    /// se declara ao codificador de AAC.
    ///
    /// # Porque é que isto tem de misturar
    ///
    /// O contentor é informado de DOIS canais. Se o dispositivo for 5.1 e se lhe passassem
    /// seis, o Media Foundation leria os mesmos bytes como três vezes mais quadros de
    /// estéreo — o som sairia ao triplo da velocidade e agudo. Não estoirava; soava mal, e
    /// só em casa de quem tem 5.1. É o tipo de avaria que nunca se reproduz onde se
    /// desenvolve.
    ///
    /// A mistura segue a receita habitual (ITU-R BS.775): a frente vai direta, o centro
    /// entra nos dois lados a -3 dB — é onde vive a voz, e deixá-lo cair era perder
    /// exactamente o que interessa — as traseiras entram também a -3 dB, e o LFE fica de
    /// fora, porque num altifalante de portátil só faria estalar.
    fn para_estereo(bytes: &[u8], forma: Forma, canais: u16) -> Vec<u8> {
        let n = canais.max(1) as usize;
        let bytes_por_amostra = match forma {
            Forma::Int16 => 2,
            Forma::Int24 => 3,
            Forma::Float32 | Forma::Int32 => 4,
            Forma::Desconhecida => return Vec::new(),
        };
        let quadros = bytes.len() / (bytes_por_amostra * n);
        let mut v = Vec::with_capacity(quadros * 4);
        // -3 dB, que é o factor com que o centro e as traseiras entram na mistura. É
        // literalmente 1/raiz(2), e o Rust já o tem escrito com todos os dígitos.
        const MEIA: f32 = std::f32::consts::FRAC_1_SQRT_2;
        for q in 0..quadros {
            let base = q * n;
            let le = |c: usize| amostra(bytes, base + c, forma).unwrap_or(0.0);
            let (mut e, mut d) = if n == 1 {
                let m = le(0);
                (m, m)
            } else {
                (le(0), le(1))
            };
            if n >= 3 {
                let centro = le(2) * MEIA;
                e += centro;
                d += centro;
            }
            // 5.1 = FL FR FC LFE BL BR — salta-se o 3 (LFE) de propósito.
            if n >= 6 {
                e += le(4) * MEIA;
                d += le(5) * MEIA;
            }
            for canal in [e, d] {
                // O clamp não é zelo a mais: somar canais passa de 1.0 com facilidade, e
                // sem isto dava a volta ao número — estalo em vez de saturar.
                let i = (canal.clamp(-1.0, 1.0) * 32767.0) as i16;
                v.extend_from_slice(&i.to_le_bytes());
            }
        }
        v
    }

    /// Como o dispositivo escreve cada amostra.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Forma {
        Float32,
        Int32,
        Int24,
        Int16,
        Desconhecida,
    }

    /// Lê a forma do `WAVEFORMATEX`, descendo ao `WAVEFORMATEXTENSIBLE` quando é preciso.
    ///
    /// O teste antigo era `wFormatTag != PCM && bits == 32` — e isso trata um dispositivo
    /// de 32 bits INTEIROS como se fosse float, o que transforma música em ruído a todo o
    /// volume. O `wFormatTag` de um formato moderno é quase sempre `EXTENSIBLE`, e quem
    /// sabe a verdade é o `SubFormat` que vem a seguir.
    unsafe fn forma_de(mix: *const WAVEFORMATEX) -> Forma {
        let bits = (*mix).wBitsPerSample;
        let etiqueta = (*mix).wFormatTag as u32;
        let float = if etiqueta == WAVE_FORMAT_EXTENSIBLE {
            let ext = mix as *const WAVEFORMATEXTENSIBLE;
            // `read_unaligned` e não `(*ext).SubFormat`: a estrutura é *packed*, e tirar uma
            // referência a um campo dela é comportamento indefinido mesmo que nunca se
            // desreferencie. O compilador recusa-o, e faz bem.
            std::ptr::read_unaligned(std::ptr::addr_of!((*ext).SubFormat))
                == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT
        } else {
            etiqueta == WAVE_FORMAT_IEEE_FLOAT
        };
        match (float, bits) {
            (true, 32) => Forma::Float32,
            (false, 32) => Forma::Int32,
            (false, 24) => Forma::Int24,
            (false, 16) => Forma::Int16,
            _ => Forma::Desconhecida,
        }
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
                canais: 2,
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
            let forma = forma_de(mix);
            if forma == Forma::Desconhecida {
                eprintln!(
                    "[som] formato de {bits} bits que não se sabe ler; a partilha vai muda \
                     em vez de ir com ruído"
                );
            }

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

            // Dois canais, sempre: é o que sai do `para_estereo` e é o que o contentor
            // declara. Anunciar os canais do dispositivo aqui era a origem do erro.
            anunciar(Formato {
                ritmo,
                canais: 2,
                bits: 16,
            });

            // JÁ EM ESTÉREO: é o que se declara ao codificador, e tem de ser o que se lhe
            // entrega. Ver `para_estereo`.
            let bytes_por_frame = 4;
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
                        let pcm = if mudo || dados.is_null() || forma == Forma::Desconhecida {
                            vec![0u8; frames as usize * bytes_por_frame]
                        } else {
                            let cru = std::slice::from_raw_parts(
                                dados,
                                frames as usize * canais as usize * (bits as usize / 8),
                            );
                            para_estereo(cru, forma, canais)
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

    /// Quem está a tocar, agora, e com que volume. Diagnóstico: quando sai som das
    /// colunas e ninguém sabe de onde, é isto que responde — o Windows guarda uma sessão
    /// de áudio por processo, com o pico de cada uma.
    pub fn quem_toca() -> Result<()> {
        use windows::core::Interface;
        use windows::Win32::Media::Audio::{IAudioSessionControl2, IAudioSessionManager2};
        use windows::Win32::System::Threading::{
            OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT,
            PROCESS_QUERY_LIMITED_INFORMATION,
        };
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let enumerador: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let dispositivo = enumerador.GetDefaultAudioEndpoint(eRender, eConsole)?;
            let gestor: IAudioSessionManager2 = dispositivo.Activate(CLSCTX_ALL, None)?;
            let sessoes = gestor.GetSessionEnumerator()?;
            let n = sessoes.GetCount()?;
            println!("[som] {n} sessões de áudio no dispositivo de saída:");
            for i in 0..n {
                let Ok(s) = sessoes.GetSession(i) else {
                    continue;
                };
                let Ok(s2) = s.cast::<IAudioSessionControl2>() else {
                    continue;
                };
                let pid = s2.GetProcessId().unwrap_or(0);
                let activa = s.GetState().map(|e| e.0).unwrap_or(0);
                let mut nome = String::from("?");
                if pid != 0 {
                    if let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
                        let mut buf = [0u16; 260];
                        let mut tam = buf.len() as u32;
                        if QueryFullProcessImageNameW(
                            h,
                            PROCESS_NAME_FORMAT(0),
                            windows::core::PWSTR(buf.as_mut_ptr()),
                            &mut tam,
                        )
                        .is_ok()
                        {
                            nome = String::from_utf16_lossy(&buf[..tam as usize]);
                        }
                        let _ = windows::Win32::Foundation::CloseHandle(h);
                    }
                }
                // estado 1 = a tocar agora; 0 = inactiva; 2 = terminada
                let estado = match activa {
                    1 => "A TOCAR",
                    0 => "parada ",
                    _ => "morta  ",
                };
                println!("  {estado} pid {pid:<7} {nome}");
            }
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

pub use win::{captar, formato, medir, quem_toca};

/// Lança a captura numa thread própria, ancorada em `origem`.
pub fn arrancar(
    parar: Arc<AtomicBool>,
    origem: std::time::Instant,
    formato: Formato,
    mut entrega: impl FnMut(Bocado) + Send + 'static,
) {
    std::thread::spawn(move || {
        match captar(parar.clone(), origem, |_| {}, &mut entrega) {
            Ok(()) => {}
            Err(e) => {
                eprintln!("[som] a captura do som falhou: {e:?}; a faixa segue em silêncio");
                // A faixa de som JÁ FOI DECLARADA no contentor — o `moov` saiu antes disto
                // poder falhar, e não há como o voltar atrás. Uma faixa declarada e nunca
                // alimentada é pior do que silêncio: quem recebe fica à espera de amostras
                // que não vêm, e o leitor pode simplesmente parar.
                //
                // Portanto alimenta-se. O silêncio custa quase nada a comprimir e mantém a
                // promessa que o cabeçalho fez.
                silencio(parar, origem, formato, entrega);
            }
        }
    });
}

/// Enche a faixa de som com silêncio, ao ritmo certo, até mandarem parar.
fn silencio(
    parar: Arc<AtomicBool>,
    origem: std::time::Instant,
    formato: Formato,
    mut entrega: impl FnMut(Bocado),
) {
    let ritmo = formato.ritmo.max(8000) as u64;
    let bytes_por_quadro = 4usize; // estéreo, 16 bits
    let mut entregues: u64 = 0;
    while !parar.load(Ordering::Relaxed) {
        let decorrido = origem.elapsed().as_secs_f64();
        let devidos = (decorrido * ritmo as f64) as u64;
        if devidos > entregues + ritmo / 50 {
            let faltam = (devidos - entregues) as usize;
            entrega(Bocado {
                pcm: vec![0u8; faltam * bytes_por_quadro],
                instante: (entregues * 10_000_000 / ritmo) as i64,
                duracao: (faltam as u64 * 10_000_000 / ritmo) as i64,
            });
            entregues = devidos;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[allow(dead_code)]
fn _usa(_: &Bocado, _: &Formato, _: &AtomicBool, _: &Ordering) {}
