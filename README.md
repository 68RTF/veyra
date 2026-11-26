<h1 align="center">
  <img src="docs/veyra-logo.svg" alt="Veyra" width="560">
</h1>

Veyra — плеер локальной музыки для Linux с очередью, обложками и текстами песен.

[Скачать Veyra 0.8.0](https://github.com/68RTF/veyra/releases/tag/v0.8.0) · [Установка](#установка) · [Сборка](#сборка)

![Veyra во время воспроизведения: обложка, очередь следующих треков и панель управления](docs/player.png)

## установка

скачай пакет со [страницы релиза](https://github.com/68RTF/veyra/releases/tag/v0.8.0) и выполни команду в каталоге с ним. сборки — для x86-64.

| Система | Команда |
| --- | --- |
| Arch Linux | `sudo pacman -U ./veyra-0.8.0-1-x86_64.pkg.tar.zst` |
| Ubuntu 24.04 | `sudo apt install ./veyra_0.8.0_ubuntu24.04_amd64.deb` |
| Debian 13 | `sudo apt install ./veyra_0.8.0_debian13_amd64.deb` |

## сборка

нужны Rust/Cargo, заголовки GTK 3 и `pkg-config`. для работы плеера нужны GTK 3, mpv, FFmpeg с `ffprobe` и curl.

```sh
cargo build --release --locked --bins
./target/release/veyra
```

## лицензия

Copyright (C) 2026 68RTF. код и оригинальные ресурсы Veyra распространяются на условиях [GPL-3.0-only](LICENSE). для сторонних компонентов действуют их собственные лицензии.
