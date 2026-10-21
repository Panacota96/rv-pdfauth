//! Prints the `PdfReport` of each file as JSON (one object per line). Used by tests/oracle_pdf.py
//! and for manual inspection: `cargo run -p rv-pdfauth-pdf --example dump -- file.pdf [--tree]`.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opts = rv_pdfauth_pdf::PdfOptions {
        object_tree: args.iter().any(|a| a == "--tree"),
        embedded: true,
        ..Default::default()
    };
    let mut status = 0;
    for path in args.iter().filter(|a| !a.starts_with("--")) {
        match std::fs::read(path) {
            Ok(bytes) => {
                let rep = rv_pdfauth_pdf::analyze(&bytes, &opts);
                let v = serde_json::json!({"file": path, "report": rep});
                println!("{}", v);
            }
            Err(e) => {
                eprintln!("{}: {}", path, e);
                status = 1;
            }
        }
    }
    std::process::exit(status);
}
