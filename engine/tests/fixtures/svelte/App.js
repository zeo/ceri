import 'svelte/internal/disclose-version';
import * as $ from 'svelte/internal/client';

var root = $.from_html(`<li> </li>`);
var root_1 = $.from_html(`<section><p id="count"> </p> <button id="increment">add one</button></section> <section><input id="draft"/> <button id="add">add</button> <button id="drop">drop first</button> <ul id="items"></ul></section>`, 1);

export default function App($$anchor) {
	let count = $.state(0);
	let items = $.proxy(["alpha", "beta"]);
	let draft = $.state("");

	function add() {
		if ($.get(draft)) {
			items.push($.get(draft));
			$.set(draft, "");
		}
	}

	var fragment = root_1();
	var section = $.first_child(fragment);
	var p = $.child(section);
	var text = $.only_child(p);
	var button = $.sibling(p, 2);

	$.reset(section);

	var section_1 = $.sibling(section, 2);
	var input = $.child(section_1);

	$.remove_input_defaults(input);

	var button_1 = $.sibling(input, 2);
	var button_2 = $.sibling(button_1, 2);
	var ul = $.sibling(button_2, 2);

	$.each(ul, 20, () => items, (item) => item, ($$anchor, item) => {
		var li = root();
		var text_1 = $.only_child(li, true);

		$.template_effect(() => $.set_text(text_1, item));
		$.append($$anchor, li);
	});

	$.reset(ul);
	$.reset(section_1);
	$.template_effect(() => $.set_text(text, `count: ${$.get(count) ?? ''}`));
	$.delegated('click', button, () => $.update(count));
	$.bind_value(input, () => $.get(draft), ($$value) => $.set(draft, $$value));
	$.delegated('click', button_1, add);
	$.delegated('click', button_2, () => items.shift());
	$.append($$anchor, fragment);
}

$.delegate(['click']);