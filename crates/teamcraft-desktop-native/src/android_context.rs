//! Android: hands the Java VM and the application context to `ndk-context`. cpal's AAudio backend
//! asks it for them on every stream it opens (the mixer's burst count through `AudioManager`), and
//! panics when nobody set them: on Android every call ended the moment its microphone opened. The
//! app calls this once, through JNI, before it joins a call.

use std::ffi::c_void;
use std::sync::Once;

use jni_sys::{JNI_OK, JNIEnv, JavaVM, jclass, jobject};

static INITIALIZED: Once = Once::new();

/// The JNI method `de.zeitgleich.teamcraft.calls.AudioContext.initialize(Context)`. Only the first
/// call counts; a missing environment or context leaves it for the next.
#[allow(unsafe_code)]
// SAFETY: the name is the JNI symbol of one native method; nothing else in the process exports it.
#[unsafe(no_mangle)]
pub extern "system" fn Java_de_zeitgleich_teamcraft_calls_AudioContext_initialize(
  env: *mut JNIEnv,
  _class: jclass,
  context: jobject,
) {
  if env.is_null() || context.is_null() {
    return;
  }
  INITIALIZED.call_once(|| {
    // SAFETY: `env` is the JNIEnv the VM passes to a native method on the calling thread; it points
    // to the VM's function table, which stays valid while the method runs.
    let functions = unsafe { &**env };
    let (Some(get_java_vm), Some(new_global_ref)) = (functions.GetJavaVM, functions.NewGlobalRef) else {
      return;
    };
    let mut vm: *mut JavaVM = std::ptr::null_mut();
    // SAFETY: a function of the VM's table called with its own `env` and a valid out pointer.
    if unsafe { get_java_vm(env, &mut vm) } != JNI_OK || vm.is_null() {
      return;
    }
    // SAFETY: `context` is a live local reference the VM passed in; the global reference outlives
    // this call and is never deleted, as ndk-context keeps it for the life of the process.
    let global = unsafe { new_global_ref(env, context) };
    if global.is_null() {
      return;
    }
    // SAFETY: `vm` is the process's JavaVM and `global` a global reference to an
    // `android.content.Context`, what ndk-context requires; `Once` makes this the only call.
    unsafe { ndk_context::initialize_android_context(vm.cast::<c_void>(), global.cast::<c_void>()) };
    tracing::info!("android audio context ready");
  });
}
