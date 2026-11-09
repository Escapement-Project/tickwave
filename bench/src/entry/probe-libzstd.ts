// bench/libzstd's probe build, with libzstd: a block's inflate and parse timed
// apart.
import * as glue from "../../build/glue/probe-libzstd/tickwave_wasm_libzstd.js";
import { wasmDecoder, type Glue } from "../decoders.ts";
import { serve } from "../worker.ts";

serve(wasmDecoder(glue as unknown as Glue, new URL("./tickwave_wasm_libzstd_bg.wasm", import.meta.url), true));
