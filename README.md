<h1 align="center">
  <img src="docs/veyra-logo.svg" alt="Veyra" width="560">
</h1>

<p align="center">
  плеер локальной музыки. тексты песен, обложки и немного винила.
</p>

<p align="center">
  <img src="assets/readme-stack.svg" alt="Linux · Rust · GTK 3 · mpv" width="344">
</p>

<p align="center">
  <a href="https://github.com/68RTF/veyra/releases/latest"><b>скачать Veyra</b></a>
  &nbsp;·&nbsp; <a href="#установка">установка</a>
  &nbsp;·&nbsp; <a href="#сборка">собрать самому</a>
</p>

![Veyra: обложка, пластинка и очередь треков](docs/player.png)

## что внутри

| <img src="assets/readme-music.svg" width="32" height="32" alt=""><br>музыка | <img src="assets/readme-lyrics.svg" width="32" height="32" alt=""><br>тексты | <img src="assets/readme-album.svg" width="32" height="32" alt=""><br>обложки |
| --- | --- | --- |
| локальные файлы, очередь, поиск и перемешивание | текущая строка подсвечивается и следует за музыкой | обложки альбомов и пластинка, которая вращается во время воспроизведения |

скорость, повтор и громкость настраиваются. очередь и настройки сохраняются между запусками.

## установка

[готовые пакеты](https://github.com/68RTF/veyra/releases/latest) для x86-64:

| система | команда |
| --- | --- |
| Arch Linux | `sudo pacman -U ./veyra-0.8.0-1-x86_64.pkg.tar.zst` |
| Ubuntu 24.04 | `sudo apt install ./veyra_0.8.0_ubuntu24.04_amd64.deb` |
| Debian 13 | `sudo apt install ./veyra_0.8.0_debian13_amd64.deb` |

## сборка

нужны Rust/Cargo, заголовки GTK 3 и `pkg-config`. для работы плеера — mpv, FFmpeg с `ffprobe` и curl.

```sh
cargo build --release --locked --bins
./target/release/veyra
```

## лицензия

Copyright (C) 2026 68RTF. [GPL-3.0-only](LICENSE). для сторонних компонентов действуют их собственные лицензии.
