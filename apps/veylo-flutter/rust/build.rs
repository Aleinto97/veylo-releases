fn main() {
    println!("cargo:rerun-if-env-changed=VEYLO_SENTRY_DSN");
    println!("cargo:rerun-if-env-changed=VEYLO_FIREBASE_API_KEY");
    println!("cargo:rerun-if-env-changed=VEYLO_FIREBASE_PROJECT");
    println!("cargo:rerun-if-env-changed=VEYLO_GOOGLE_WEB_CLIENT");
}
