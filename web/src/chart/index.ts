// tickwave's charts. Importing this module defines <tickwave-chart>. It loads nothing else until a
// reader opens a point's recording.

import { TickwaveChart } from "./element.ts";

export { TickwaveChart };
export type { Reserved } from "./element.ts";
export { parseChart, type ChartData } from "./data.ts";

if (!customElements.get("tickwave-chart")) customElements.define("tickwave-chart", TickwaveChart);
