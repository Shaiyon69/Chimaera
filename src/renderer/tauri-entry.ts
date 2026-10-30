// Tauri entry: same renderer, with the Electron preload run in-page first.
// Import order is execution order here, and each step needs the one before.
import "../tauri/install-globals";
import "../preload/index";
import "./src/main";
