//! Bruma — um Discord P2P, anónimo e cifrado ponta a ponta, sem servidor.
//!
//! O arranque é deliberadamente simples: carrega (ou cria) a identidade, abre a rede, e
//! entrega os dois ao Tauri. Não há login, não há registo, não há nada a contactar.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod comandos;
mod ecra;
mod estado;
mod fmp4;
mod jogo;
mod modelo;
mod mse;
mod rede;

use std::sync::Arc;
use tauri::Manager;

fn main() {
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

    if std::env::args().any(|a| a == "--autoteste") {
        mse::VER_CAIXAS.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .setup(|app| {
            let nucleo = Arc::new(estado::App::arrancar()?);
            let ecra = Arc::new(comandos::Ecra::default());
            let _ = comandos::ECRA.set(ecra.clone());
            app.manage(ecra);
            let janela = app.handle().clone();

            // A rede precisa de um runtime async; o Tauri já traz um.
            let rede = tauri::async_runtime::block_on(rede::Rede::arrancar(
                nucleo.clone(),
                janela.clone(),
            ))?;

            println!("Bruma pronto.");
            println!("  dados      : {}", estado::raiz().display());
            println!("  identidade : {}", nucleo.minha_chave());
            println!("  endereço   : {}", rede.id());

            app.manage(nucleo);
            app.manage(rede);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            comandos::estado,
            comandos::definir_nome,
            comandos::criar_servidor,
            comandos::criar_canal,
            comandos::apagar_canal,
            comandos::criar_convite,
            comandos::entrar_com_convite,
            comandos::enviar,
            comandos::mensagens,
            comandos::presenca_de_voz,
            comandos::enviar_sinal,
            comandos::meu_endereco,
            comandos::saude,
            comandos::capacidades,
            comandos::autoteste_pedido,
            comandos::receber_ecra,
            comandos::comecar_a_partilhar,
            comandos::parar_de_partilhar,
            comandos::definir_espectadores,
            jogo::jogo_em_execucao,
        ])
        .run(tauri::generate_context!())
        .expect("falha a arrancar o Bruma");
}
