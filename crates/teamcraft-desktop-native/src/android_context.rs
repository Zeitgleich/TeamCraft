//! Android: hands the Java VM and the application context to `ndk-context`. cpal's AAudio backend
//! asks it for them on every stream it opens (the mixer's burst count through `AudioManager`), and
//! panics when nobody set them: on Android every call ended the moment its microphone opened. The
//! app calls this through JNI before it joins a call, until it succeeded once.

use std::ffi::c_void;
use std::sync::{Mutex, PoisonError};

use jni_sys::{JNI_FALSE, JNI_OK, JNI_TRUE, JNIEnv, JavaVM, jboolean, jclass, jobject};

/// Whether ndk-context has its VM and context. Set only once that succeeded: a call that failed
/// (no environment, no global reference) leaves it to the next, which tries again.
static IS_INITIALIZED: Mutex<bool> = Mutex::new(false);

/// The JNI method `de.zeitgleich.teamcraft.calls.AudioContext.initialize(Context): Boolean`.
/// Returns whether the context is set up, by this call or an earlier one; on `false` the app tries
/// again before the next call.
#[allow(unsafe_code)]
// SAFETY: the name is the JNI symbol of one native method; nothing else in the process exports it.
#[unsafe(no_mangle)]
pub extern "system" fn Java_de_zeitgleich_teamcraft_calls_AudioContext_initialize(
  env: *mut JNIEnv,
  _class: jclass,
  context: jobject,
) -> jboolean {
  // Held for the whole attempt, so two threads never both hand a context to ndk-context.
  let mut is_initialized = IS_INITIALIZED.lock().unwrap_or_else(PoisonError::into_inner);
  if !*is_initialized {
    *is_initialized = initialize(env, context);
  }
  if *is_initialized { JNI_TRUE } else { JNI_FALSE }
}

/// One attempt; `true` once ndk-context has the VM and a global reference to `context`.
#[allow(unsafe_code)]
fn initialize(env: *mut JNIEnv, context: jobject) -> bool {
  if env.is_null() || context.is_null() {
    return false;
  }
  // SAFETY: `env` is the JNIEnv the VM passes to a native method on the calling thread; it points
  // to the VM's function table, which stays valid while the method runs.
  let functions = unsafe { &**env };
  let (Some(get_java_vm), Some(new_global_ref)) = (functions.GetJavaVM, functions.NewGlobalRef) else {
    return false;
  };
  let mut vm: *mut JavaVM = std::ptr::null_mut();
  // SAFETY: a function of the VM's table called with its own `env` and a valid out pointer.
  if unsafe { get_java_vm(env, &mut vm) } != JNI_OK || vm.is_null() {
    return false;
  }
  // SAFETY: `context` is a live local reference the VM passed in; the global reference outlives
  // this call and is never deleted, as ndk-context keeps it for the life of the process.
  let global = unsafe { new_global_ref(env, context) };
  if global.is_null() {
    return false;
  }
  // SAFETY: `vm` is the process's JavaVM and `global` a global reference to an
  // `android.content.Context`, what ndk-context requires. The caller holds `IS_INITIALIZED` and
  // calls this only while it is false, so this runs at most once successfully.
  unsafe { ndk_context::initialize_android_context(vm.cast::<c_void>(), global.cast::<c_void>()) };
  tracing::info!("android audio context ready");
  true
}
