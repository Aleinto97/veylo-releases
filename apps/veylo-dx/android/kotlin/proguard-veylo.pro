# Keep Veylo's Kotlin JNI bridges loaded dynamically via ClassLoader.loadClass from Rust (src/android.rs).
-keep class dev.veylo.dx.** {
  *;
}
