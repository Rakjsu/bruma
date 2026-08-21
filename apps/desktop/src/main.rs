#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod modelo;

fn main() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("falha a arrancar o Bruma");
}
