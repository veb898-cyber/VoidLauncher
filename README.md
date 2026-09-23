# VoidLauncher

A modern, open-source Minecraft launcher for Windows.


VoidLauncher is a desktop launcher built around a simple idea: keep Minecraft versions, modded installations, configuration, and multiplayer in one place without turning the launcher itself into a complicated system.


Each Minecraft installation is managed independently, with its own game version, mod loader, Java configuration, content, and settings. The launcher handles the setup and management around Minecraft while keeping the actual game installations separate and easy to work with.


VoidLauncher is built with a native Rust backend and a React-based interface using Tauri 2. The project is designed primarily for Windows and focuses on a lightweight desktop experience rather than replicating the interface or architecture of an existing launcher.


## Getting Started


Download the latest release from the [Releases](../../releases) page and run the Windows installer.


VoidLauncher uses a per-user installation, so administrator privileges are not required for a normal installation.


## Building


### Requirements


* Windows 10/11 (x86_64)
* Node.js 24+
* Rust (stable)
* Tauri 2 prerequisites
* WebView2


### Build from source


```bash
npm install
npm run tauri build
```


## Technology


VoidLauncher is built with:


* [Tauri 2](https://tauri.app/)
* Rust
* React 19
* TypeScript
* Vite
* Tokio


The application keeps the desktop-side game management and system integration in Rust while the user interface is implemented with React.


## Project


VoidLauncher is an independent open-source project developed around Minecraft and its surrounding ecosystem.


The project is still evolving, and its architecture and user experience may change as development continues.


## License


VoidLauncher is licensed under the [MIT License](LICENSE).


You are free to use, modify, and redistribute the project in accordance with the license terms.


## Author


[veb898-cyber](https://github.com/veb898-cyber)
