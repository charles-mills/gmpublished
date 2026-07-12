## About

gmpublished is a web-view free fork of William Venner's [gmpublisher](https://github.com/WilliamVenner/gmpublisher), built with [Iced](https://iced.rs/).

On top of the features offered by (and thanks to) gmpublisher, it includes full in-app preview tools for maps, models, code, materials and audio. Click [here](#compared-to-gmpublisher) for a more detailed comparison. Note that most additional features can be removed by building with --no-default-features.

## Installation

| Platform     | Method           |
| ------------ | ---------------- |
| Windows      | Download and run the **.msi** from the latest [release](https://github.com/charles-mills/gmpublished/releases). |
| macOS        | Download and run the **.dmg** from the latest [release](https://github.com/charles-mills/gmpublished/releases). |
| Linux        | Download a **.appimage**, **.deb**, or the binary from the latest [release](https://github.com/charles-mills/gmpublished/releases). |

Alternatively, a Nix flake is available for macOS and Linux. For example, to build and run the app without installing:

```bash
nix run github:charles-mills/gmpublished
```

## Media

<img alt="the workshop page" src="https://github.com/user-attachments/assets/b30afbe6-3519-46ea-9600-594187207aae" />
<img alt="the addon update modal" src="https://github.com/user-attachments/assets/50b7af77-b324-4edb-8dd4-5d64900b7a14" />
<img alt="the search modal" src="https://github.com/user-attachments/assets/25236541-45f8-4b8f-b9e2-52a1edc69679" />
<img alt="the addon size analyzer" src="https://github.com/user-attachments/assets/75716efa-4e85-4a8b-8e34-d61b774a7d37" />
<img alt="the model previewer" src="https://github.com/user-attachments/assets/c882fb02-c5ab-40ea-a583-68e3d4d8af3d" />
<img alt="the map previewer (1/2)" src="https://github.com/user-attachments/assets/aabbe28f-0fe1-4037-bb1d-eb374dfc1265" />
<img alt="the map previewer (2/2)" src="https://github.com/user-attachments/assets/716afb57-5315-4fc6-aa07-720b955ae6f8" />
<img alt="the code previewer" src="https://github.com/user-attachments/assets/f0f09340-7c28-4d85-9ae7-e580b27eb18d" />

## Compared to gmpublisher

### UI Framework

gmpublished is implemented in [Iced](https://iced.rs/), and so doesn't require a system or bundled web-view, instead rendering with [wgpu](https://github.com/gfx-rs/wgpu). This is mostly relevant to Linux users, since gmpublisher's Tauri dependency is not regularly updated to use a widely available version of webkitgtk.

Whilst a little awkward, it's still perfectly possible to use gmpublisher on the vast majority of Linux distros, such as through the [Arch User Repository](https://aur.archlinux.org/packages/gmpublisher-bin), or by installing an archived libwebkit2gtk-4.0-37 package.

### In-app Previewer

All addons can be previewed in the app, including their map files, models, textures, sounds, and code. When previewing maps, both walk and fly modes are supported, including jump and crouch; standard gmod controls are used (v to toggle fly, et cetera).

### UI and Navigation

All navigation and app control happens in the sidebar to give more space to content. You can continue to access settings, search, and the various pages from the sidebar.

| Shortcut       | Action              |
| -------------- | ------------------- |
| **CTRL / ⌘ ,** | Settings            |
| **CTRL / ⌘ F** | Addon Search        |
| **CTRL / ⌘ K** | File Search         |
| **CTRL / ⌘ O** | Open GMA            |
| **CTRL / ⌘ 1** | My Workshop         |
| **CTRL / ⌘ 2** | Installed Addons    |
| **CTRL / ⌘ 3** | Downloader          |
| **CTRL / ⌘ 4** | Addon Size Analyzer |

### Additionals

- Drag & drop an addon from anywhere into the Downloader tab.
- Installed addons are updated as you remove or subscribe to addons.
- App themes, including auto OS theme setting.
