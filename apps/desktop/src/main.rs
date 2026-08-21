//! Bruma — um Discord P2P, anónimo e cifrado ponta a ponta, sem servidor.
//!
//! O arranque é deliberadamente simples: carrega (ou cria) a identidade, abre a rede, e
//! entrega os dois ao Tauri. Não há login, não há registo, não há nada a contactar.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod comandos;
mod estado;
mod jogo;
mod modelo;
mod rede;

use std::sync::Arc;
use tauri::Manager;

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .setup(|app| {
            let nucleo = Arc::new(estado::App::arrancar()?);
            let janela = app.handle().clone();

            // A rede precisa de um runtime async; o Tauri já traz um.
            let rede = tauri::async_runtime::block_on(rede::Rede::arrancar(
                nucleo.clone(),
                janela.clone(),
            ))?;

            println!("Bruma pronto.");
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
            jogo::jogo_em_execucao,
        ])
        .run(tauri::generate_context!())
        .expect("falha a arrancar o Bruma");
}
