// drives svelte 5's real prebundled dev-runtime far enough to schedule and resolve a batch
// whose effect has a parent, so the scheduler walks the parent chain

import {
	a as get,
	C as effect_root,
	dt as set,
	F as user_effect,
	pt as state,
} from "./runtime-Do_u7Ssq.js";

const outer = state(0);
const inner = state(0);
effect_root(() => {
	user_effect(() => {
		get(outer);
		user_effect(() => {
			document.getElementById("status").textContent = `seen ${get(inner)}`;
		});
	});
});
set(inner, 1);
