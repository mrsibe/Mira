use tauri::Manager;

mod cancellation;
mod chat;
mod database;
mod memory;
mod model;
mod runtime;
mod secrets;
mod types;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            let conn = database::init_database(app.handle())
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::Other, error))?;
            app.manage(database::DbState {
                conn: std::sync::Mutex::new(conn),
            });
            app.manage(cancellation::CancellationState::new());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            cancellation::cancel_message,
            chat::send_message,
            chat::list_conversations,
            chat::list_archived_conversations,
            chat::get_conversation_messages,
            chat::create_conversation,
            chat::archive_conversation,
            chat::restore_conversation,
            chat::delete_conversation,
            chat::move_conversation_to_project,
            chat::list_projects,
            chat::create_project,
            chat::delete_project,
            chat::rename_project,
            chat::rename_conversation,
            chat::get_system_prompt,
            chat::save_system_prompt,
            chat::list_model_configs,
            chat::save_model_config,
            chat::delete_model_config,
            chat::get_model_api_key,
            chat::get_model_settings,
            chat::save_model_settings,
            chat::list_memories,
            chat::create_saved_memory,
            chat::update_memory,
            chat::delete_memory,
            chat::run_memory_cleanup,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
