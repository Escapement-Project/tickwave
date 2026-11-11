// escapement-tickwave-format's block parser with libzstd compiled to wasm32 (bench/libzstd).
import * as glue from "../../build/glue/libzstd/escapement_tickwave_wasm_libzstd.js";
import { wasmDecoder, type Glue } from "../decoders.ts";
import { serve } from "../worker.ts";

serve(wasmDecoder(glue as unknown as Glue, new URL("./escapement_tickwave_wasm_libzstd_bg.wasm", import.meta.url)));
