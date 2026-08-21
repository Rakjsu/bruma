//! Spike 2 - partilha de ecra dentro do Tauri.
//!
//! O Rust aqui e so a casca: abre a janela e deixa o WebView2 fazer o trabalho.
//! Toda a medicao esta em ui/index.html, porque a pergunta e precisamente
//! "o que e que a webview consegue fazer sozinha".

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("falha a arrancar a aplicacao Tauri");
}
