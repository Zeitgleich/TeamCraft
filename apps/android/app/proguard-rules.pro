# UniFFI bindings use JNA, which looks classes up by name.
-keep class com.sun.jna.** { *; }
-keep class * implements com.sun.jna.** { *; }
-keep class uniffi.teamcraft_ffi.** { *; }
-dontwarn java.awt.**
