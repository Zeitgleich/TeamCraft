// The segmentation worker's script (#439): loads the segmentation module, whose start function
// takes the call module's messages (teamcraft-web-segmentation, src/lib.rs). The build names the
// module after its content (crates/teamcraft-ui/scripts/build-segmentation.sh) and this script too.
import init from '__SEGMENTATION_MODULE__';

init().catch((error) => {
  // A module that can't start (no WebAssembly SIMD, say): the page hears why and goes on without
  // the effects.
  self.postMessage({ kind: 'failed', detail: `the segmentation module didn't start: ${error}` });
});
