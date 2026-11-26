#[path = "../yandex_export.rs"]
mod yandex_export;

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if let Err(error) = yandex_export::run(&args) {
        eprintln!("Не удалось экспортировать плейлист: {error}");
        std::process::exit(1);
    }
}
