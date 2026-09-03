//! Bruma — um Discord P2P, anónimo e cifrado ponta a ponta, sem servidor.
//!
//! O arranque é deliberadamente simples: carrega (ou cria) a identidade, abre a rede, e
//! entrega os dois ao Tauri. Não há login, não há registo, não há nada a contactar.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bandeiras;
mod comandos;
mod ecra;
mod estado;
mod fmp4;
mod fontes;
mod jogo;
mod modelo;
mod mse;
mod rede;
mod registo;
mod som;

use std::sync::Arc;
use tauri::Manager;

/// Responde aos pedidos de permissão do WebView2 para a câmara e o microfone.
///
/// # Porque é que isto tem de existir
///
/// O `wry` regista um tratador de permissões — mas só para a área de transferência. Para a
/// câmara e o microfone não regista nenhum, e um pedido sem quem lhe responda **fica
/// pendurado para sempre**: o `getUserMedia` nunca resolve nem rejeita. Medido: com uma
/// câmara presente e a funcionar, o pedido não respondeu em 6 segundos, e não ia responder
/// aos 6 minutos.
///
/// O microfone parecia estar bem, e não estava — estava só a aproveitar uma autorização
/// que este WebView2 tinha guardada de uma sessão antiga. Numa máquina onde ninguém tenha
/// autorizado nada, a voz teria falhado exactamente da mesma maneira, em silêncio, e o
/// sintoma seria "o Bruma não tem som" numa casa e não na outra. É por isso que os DOIS
/// entram aqui, e não só a câmara que deu o erro.
///
/// Autorizar sem perguntar é o certo neste sítio: a pergunta já foi feita — a pessoa
/// carregou no botão da câmara ou entrou numa chamada. Uma segunda caixa a perguntar o
/// mesmo não acrescenta escolha nenhuma, e o Windows já mostra o indicador de câmara e
/// microfone em uso na barra de tarefas, que é onde uma garantia dessas se deve ver.
#[cfg(windows)]
fn responder_a_permissoes(janela: &tauri::WebviewWindow) {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_PERMISSION_KIND, COREWEBVIEW2_PERMISSION_KIND_CAMERA,
        COREWEBVIEW2_PERMISSION_KIND_MICROPHONE, COREWEBVIEW2_PERMISSION_STATE_ALLOW,
    };
    use webview2_com::PermissionRequestedEventHandler;

    let r = janela.with_webview(|w| unsafe {
        let controlador = w.controller();
        let Ok(nucleo) = controlador.CoreWebView2() else {
            eprintln!("[permissões] sem acesso ao WebView2; a câmara pode não abrir");
            return;
        };
        let mut token = 0i64;
        let resultado = nucleo.add_PermissionRequested(
            &PermissionRequestedEventHandler::create(Box::new(|_, args| {
                let Some(args) = args else { return Ok(()) };
                let mut tipo = COREWEBVIEW2_PERMISSION_KIND::default();
                args.PermissionKind(&mut tipo)?;
                if tipo == COREWEBVIEW2_PERMISSION_KIND_CAMERA
                    || tipo == COREWEBVIEW2_PERMISSION_KIND_MICROPHONE
                {
                    args.SetState(COREWEBVIEW2_PERMISSION_STATE_ALLOW)?;
                }
                Ok(())
            })),
            &mut token,
        );
        if let Err(e) = resultado {
            eprintln!("[permissões] não consegui responder aos pedidos: {e:?}");
        }
    });
    if let Err(e) = r {
        eprintln!("[permissões] a janela não deixou chegar ao WebView2: {e:?}");
    }
}

#[cfg(not(windows))]
fn responder_a_permissoes(_janela: &tauri::WebviewWindow) {}

/// A lista de comandos, escrita UMA vez, com um buraco para os que só existem em debug.
///
/// Duas listas copiadas seriam a receita para um comando novo funcionar em debug e faltar na
/// release — e o sintoma seria «essa funcionalidade não faz nada na versão instalada», sem
/// erro nenhum. Aqui a lista comum tem um só sítio.
///
/// Os extras vêm à frente e cada um com a sua vírgula (`$($extra,)*`), para que a lista com
/// zero extras não fique com uma vírgula pendurada.
macro_rules! handler_com {
    ($($extra:path),* $(,)?) => {
        tauri::generate_handler![
            $($extra,)*
            comandos::estado,
            comandos::definir_nome,
            comandos::criar_servidor,
            comandos::criar_canal,
            comandos::apagar_canal,
            comandos::apagar_conversa,
            comandos::sair_do_servidor,
            comandos::reabrir_canal,
            comandos::criar_convite,
            comandos::entrar_com_convite,
            comandos::abrir_conversa,
            comandos::amigos,
            comandos::adicionar_amigo,
            comandos::remover_amigo,
            comandos::marcar_verificado,
            comandos::numero_de_seguranca,
            comandos::permissoes,
            comandos::bloquear,
            comandos::definir_quem_escreve,
            comandos::marcar_lido,
            comandos::enviar,
            comandos::mensagens,
            comandos::abrir_canal,
            comandos::marcar_lido_ate,
            comandos::presenca_de_voz,
            comandos::enviar_sinal,
            comandos::meu_endereco,
            comandos::inventario,
            comandos::capacidades,
            comandos::autoteste_fps,
            comandos::autoteste_altura,
            comandos::autoteste_fonte,
            comandos::medir_som,
            comandos::sobre_esta_instalacao,
            comandos::actualizacao_incompleta,
            comandos::abrir_pasta_de_dados,
            comandos::abrir_ligacao,
            comandos::palavras_da_identidade,
            comandos::restaurar_identidade,
            comandos::receber_ecra,
            comandos::receber_camara,
            comandos::enviar_camara,
            comandos::receber_voz,
            comandos::enviar_voz,
            comandos::qualidade,
            comandos::ligacoes,
            comandos::entrada_na_rede,
            comandos::diario_da_rede,
            comandos::comecar_a_partilhar,
            comandos::parar_de_partilhar,
            fontes::fontes_de_partilha,
            comandos::ver_meu_ecra,
            comandos::parar_de_ver_meu_ecra,
            comandos::definir_espectadores,
            jogo::jogo_em_execucao
        ]
    };
}

/// Os comandos de MEDIÇÃO não vão na release.
///
/// O `convites_de_teste` existe para FABRICAR convites envenenados e o
/// `escapou_alguma_coisa` para vasculhar a pasta de dados. Ficavam registados no binário que
/// vai para as pessoas — a mesma incoerência que corrigi nas variáveis de ambiente, deixada
/// ao lado. Só são alcançáveis a partir da própria janela da app, portanto o risco é
/// pequeno; mas código que existe só para produzir cargas de ataque não tem nada que ir numa
/// release, e a lista de comandos é uma superfície que se mantém curta de propósito.
#[cfg(debug_assertions)]
fn handler_de_comandos() -> impl Fn(tauri::ipc::Invoke) -> bool + Send + Sync + 'static {
    handler_com![
        comandos::convites_de_teste,
        comandos::escapou_alguma_coisa,
        // E os três que LIGAM os guiões de medição.
        //
        // `bruma.exe --medir-ui` numa instalação a sério fazia o guião correr na pasta de
        // dados de quem instalou: acrescentar amigos, bloquear pessoas, mudar a política de
        // quem pode escrever. Um andaime que escreve estado permanente na máquina de outra
        // pessoa não é uma ferramenta, é uma armadilha — e eu tinha acabado de tirar os
        // outros e deixado estes.
        //
        // Todas as chamadas do lado do JS já têm `.catch`, portanto sem os comandos o guião
        // simplesmente não arranca. Não é preciso mexer na interface.
        comandos::autoteste_pedido,
        comandos::autoteste_par,
        comandos::medir_ui_pedido,
        // E os contadores de medição do log (#154, #90) e os tempos do arranque (#99).
        comandos::contadores,
        comandos::tempos_de_arranque
    ]
}

#[cfg(not(debug_assertions))]
fn handler_de_comandos() -> impl Fn(tauri::ipc::Invoke) -> bool + Send + Sync + 'static {
    handler_com![]
}

/// Mostra uma janela nativa com a razão de o Bruma não ter arrancado, e sai.
///
/// Sem consola (é uma app de janela) e sem a WebView (que ainda não existe quando o arranque
/// falha), esta é a única forma de dizer alguma coisa a quem está à frente do ecrã. A
/// alternativa era o comportamento antigo: a janela pisca e desaparece, e o único vestígio é
/// uma linha no bruma.log que ninguém sabe que existe.
fn avisar_e_sair(razao: &str) -> ! {
    let registo = crate::registo::caminho();
    let texto = format!(
        "O Bruma não conseguiu abrir.

{razao}

Os teus dados NÃO foram apagados.          Há mais detalhes em:
{}",
        registo.display()
    );
    eprintln!("[arranque] {razao}");
    #[cfg(windows)]
    unsafe {
        use windows::core::HSTRING;
        use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
        MessageBoxW(
            None,
            &HSTRING::from(texto.as_str()),
            &HSTRING::from("Bruma"),
            MB_OK | MB_ICONERROR,
        );
    }
    std::process::exit(1);
}

/// O relógio do arranque (#99): posto na PRIMEIRA linha de `main`, lido no `setup` depois
/// do núcleo e depois da rede — os dois passos que prendem a thread da interface com a
/// janela criada mas sem pintar. Vai para o registo como `[arranque] …` e, em debug, para o
/// comando `tempos_de_arranque` que o `--medir-ui` junta aos carimbos do JS.
pub static ARRANQUE: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
pub static ARRANQUE_INICIO_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static ARRANQUE_ATE_SETUP_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub static ARRANQUE_NUCLEO_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static ARRANQUE_REDE_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn main() {
    let _ = ARRANQUE.set(std::time::Instant::now());
    ARRANQUE_INICIO_MS.store(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
        std::sync::atomic::Ordering::Relaxed,
    );
    // A PRIMEIRA coisa: o `std` guarda o descritor de saída na primeira utilização e não
    // volta a perguntar, portanto isto tem de acontecer antes de alguém escrever seja o
    // que for. Ver `registo.rs`.
    registo::arrancar();

    // `bruma --que-jogo` responde o que o detetor vê neste momento e sai. A deteção é um
    // palpite sobre as janelas abertas, portanto quando alguém disser "apareceu-me a coisa
    // errada na barra" isto responde sem ser preciso montar nada.
    if std::env::args().any(|a| a == "--que-jogo") {
        match jogo::jogo_em_execucao() {
            Some(j) => println!(
                "{} ({}) — pontuação {:.2}",
                j.titulo, j.processo, j.cobertura
            ),
            None => println!("nada detetado"),
        }
        return;
    }

    // `bruma --ouvir=5` capta o som que sai das colunas durante cinco segundos e diz o que
    // ouviu: ritmo, canais, pico e volume médio. Foi assim que se provou que o silêncio tem
    // de ser fabricado — em loopback, quando nada toca o Windows não entrega silêncio,
    // não entrega NADA.
    if let Some(n) = std::env::args().find_map(|a| a.strip_prefix("--ouvir=").map(str::to_owned)) {
        let s: u64 = n.parse().unwrap_or(6);
        if let Err(e) = som::medir(s) {
            eprintln!("[som] {e:?}");
        }
        return;
    }
    // `bruma --quem-toca` diz que processos estão a produzir som agora. Nasceu de uma
    // pergunta prática — "de onde vem este som?" — e responde-a em duas linhas.
    if std::env::args().any(|a| a == "--quem-toca") {
        if let Err(e) = som::quem_toca() {
            eprintln!("[som] {e:?}");
        }
        return;
    }
    // `bruma --fontes` despeja o que o seletor de partilha mostraria, com as miniaturas,
    // para %TEMP%ruma-fontes.json. Serve para inspecionar o menu sem abrir a app —
    // e foi como se provou que os BMP desenhados à mão renderizam mesmo num <img>.
    if std::env::args().any(|a| a == "--fontes") {
        let fontes = fontes::fontes_de_partilha();
        let destino = std::env::temp_dir().join("bruma-fontes.json");
        std::fs::write(&destino, serde_json::to_vec(&fontes).unwrap()).unwrap();
        println!("{} fontes em {}", fontes.len(), destino.display());
        return;
    }

    if std::env::args().any(|a| a.starts_with("--autoteste")) {
        mse::VER_CAIXAS.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            // ARRANCAR SEM MORRER EM SILENCIO (#14).
            //
            // O `ler_indice` já não mata a app por causa do índice. Mas restam erros de
            // arranque — a pasta de dados inacessível, a semente corrompida — que subiam até
            // ao `.expect()` do `run()` e, com `windows_subsystem = "windows"`, faziam a
            // janela abrir, piscar e desaparecer. A pior falha silenciosa que este projecto
            // tem. Se o arranque falha, mostra-se uma janela nativa com a razão e o caminho do
            // registo, e sai-se com dignidade.
            // Até aqui foi o Tauri (o runtime, os plugins, a janela e o WebView2); daqui
            // para a frente é nosso, e mede-se à parte.
            let ate_setup_ms = ARRANQUE
                .get()
                .map(|t| t.elapsed().as_millis() as u64)
                .unwrap_or(0);
            ARRANQUE_ATE_SETUP_MS.store(ate_setup_ms, std::sync::atomic::Ordering::Relaxed);
            let relogio = std::time::Instant::now();
            let nucleo = match estado::App::arrancar() {
                Ok(n) => Arc::new(n),
                Err(e) => avisar_e_sair(&format!("{e}")),
            };
            let nucleo_ms = relogio.elapsed().as_millis() as u64;
            ARRANQUE_NUCLEO_MS.store(nucleo_ms, std::sync::atomic::Ordering::Relaxed);
            let ecra = Arc::new(comandos::Ecra::default());
            let _ = comandos::ECRA.set(ecra.clone());
            app.manage(ecra);
            let voz = Arc::new(comandos::Voz::default());
            let _ = comandos::VOZ.set(voz.clone());
            app.manage(voz);
            let camara = Arc::new(comandos::Camara::default());
            let _ = comandos::CAMARA.set(camara.clone());
            app.manage(camara);
            let janela = app.handle().clone();
            if let Some(j) = app.get_webview_window("main") {
                responder_a_permissoes(&j);
            }

            // A rede precisa de um runtime async; o Tauri já traz um.
            let rede = tauri::async_runtime::block_on(rede::Rede::arrancar(
                nucleo.clone(),
                janela.clone(),
            ))?;
            let rede_ms = (relogio.elapsed().as_millis() as u64).saturating_sub(nucleo_ms);
            ARRANQUE_REDE_MS.store(rede_ms, std::sync::atomic::Ordering::Relaxed);
            // O arranque medido antes de o mexer (#99): até aqui a janela existe mas não
            // pintou. Um número no registo, para se decidir com ele e não com um palpite.
            eprintln!(
                "[arranque] até ao setup {} ms · núcleo {} ms ({} servidores) · rede {} ms",
                ate_setup_ms,
                nucleo_ms,
                nucleo.servidores.lock().map(|s| s.len()).unwrap_or(0),
                rede_ms
            );

            println!("Bruma pronto.");
            println!("  dados      : {}", estado::raiz().display());
            println!("  identidade : {}", nucleo.minha_chave());
            println!(
                "  nome       : {}",
                nucleo.nome.lock().map(|n| n.clone()).unwrap_or_default()
            );
            println!(
                "  servidores : {}",
                nucleo.servidores.lock().map(|s| s.len()).unwrap_or(0)
            );
            println!("  endereço   : {}", rede.id());

            app.manage(nucleo);
            app.manage(rede);
            Ok(())
        })
        .invoke_handler(handler_de_comandos())
        .run(tauri::generate_context!())
        .expect("falha a arrancar o Bruma");
}
