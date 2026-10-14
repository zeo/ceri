use ceri_engine::{Document, ScriptRuntime};
use std::cell::RefCell;
use std::rc::Rc;

fn run(source: &str) -> Result<String, String> {
    let document = Document::parse("<p id='status'></p>").expect("document");
    let mut runtime = ScriptRuntime::new(Rc::new(RefCell::new(document)));
    runtime
        .execute(source)
        .map_err(|error| error.message.clone())?;
    Ok(runtime.logs().join("\n"))
}

fn check(cases: &[(&str, &str, &str)]) {
    let mut failures = Vec::new();
    for (name, source, expected) in cases {
        match std::panic::catch_unwind(|| run(source)) {
            Ok(Ok(output)) if output == *expected => {}
            Ok(Ok(output)) => {
                failures.push(format!("{name}: expected {expected:?}, got {output:?}"))
            }
            Ok(Err(error)) => failures.push(format!("{name}: failed with {error}")),
            Err(_) => failures.push(format!("{name}: panicked")),
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[test]
fn operators_follow_the_specification() {
    check(&[
        (
            "remainder",
            "console.log(7 % 3, -7 % 3, 5.5 % 2)",
            "1 -1 1.5",
        ),
        (
            "exponent",
            "console.log(2 ** 10, (-2) ** 3, 2 ** -1, 1 ** NaN)",
            "1024 -8 0.5 NaN",
        ),
        (
            "bitwise",
            "console.log(5 & 3, 5 | 3, 5 ^ 3, ~5, 1 << 31, -16 >> 2, -16 >>> 28)",
            "1 7 6 -6 -2147483648 -4 15",
        ),
        (
            "increment",
            "let a = 1; const b = a++; const c = ++a; console.log(a, b, c)",
            "3 1 3",
        ),
        (
            "compound",
            "let x = 10; x += 5; x -= 3; x *= 2; x /= 4; x %= 4; x **= 3; console.log(x)",
            "8",
        ),
        (
            "logical assignment",
            "let a = null; a ??= 1; let b = 0; b ||= 2; let c = 3; c &&= 4; console.log(a, b, c)",
            "1 2 4",
        ),
        (
            "nullish",
            "console.log(null ?? 'd', 0 ?? 'd', undefined ?? false, '' || 'or')",
            "d 0 false or",
        ),
        (
            "optional chain",
            "const o = { a: { b: () => 'hi' } }; console.log(o?.a?.b(), o.x?.y.z, o.x?.(), o.a?.['b']?.())",
            "hi undefined undefined hi",
        ),
        ("comma", "let i = (1, 2, 3); console.log(i)", "3"),
        (
            "typeof",
            "console.log(typeof 1, typeof 'a', typeof null, typeof undefined, typeof {}, typeof (() => 1), typeof Symbol(), typeof undeclared)",
            "number string object undefined object function symbol undefined",
        ),
        (
            "void and delete",
            "const o = { a: 1, b: 2 }; console.log(void 0, delete o.a, 'a' in o, 'b' in o)",
            "undefined true false true",
        ),
        (
            "loose equality",
            "console.log(1 == '1', 0 == '', null == undefined, null == 0, NaN == NaN, [1] == 1, '1,2' == [1, 2])",
            "true true true false false true true",
        ),
        (
            "strict equality",
            "console.log(1 === 1.0, '1' === 1, NaN === NaN, 0 === -0)",
            "true false false true",
        ),
        (
            "string comparison",
            "console.log('a' < 'b', 'B' < 'a', '10' < '9', 10 < 9, '10' < 9)",
            "true true true false false",
        ),
        (
            "addition coercion",
            "console.log(1 + '2', '3' * '4', true + 1, [] + {}, null + 1, undefined + 1, [1, 2] + [3])",
            "12 12 2 [object Object] 1 NaN 1,23",
        ),
        (
            "number formatting",
            "console.log(0.1 + 0.2, 1e21, 123456789012345680000, 1 / 3, -0, 2 ** 53, 5e-7)",
            "0.30000000000000004 1e+21 123456789012345680000 0.3333333333333333 0 9007199254740992 5e-7",
        ),
        (
            "number parsing",
            "console.log(+'  42 ', +'0x1f', +'1e3', +'', +'abc', +[], +[7], +null, +true)",
            "42 31 1000 0 NaN 0 7 0 1",
        ),
        (
            "conditional",
            "console.log(1 ? 'a' : 'b', 0 ? 'a' : '' ? 'b' : 'c')",
            "a c",
        ),
        (
            "instanceof",
            "class A {} class B extends A {} const b = new B(); console.log(b instanceof B, b instanceof A, b instanceof Object, [] instanceof Array, {} instanceof Array)",
            "true true true true false",
        ),
        (
            "valueOf and toString",
            "const money = { valueOf() { return 5; }, toString() { return 'five'; } }; console.log(money + 1, `${money}`, money * 2, String(money))",
            "6 five 10 five",
        ),
        (
            "in with arrays",
            "console.log(0 in [1], 1 in [1], 'length' in [])",
            "true false true",
        ),
    ]);
}

#[test]
fn statements_and_control_flow_work() {
    check(&[
        (
            "for loop",
            "let s = ''; for (let i = 0; i < 5; i++) { if (i === 1) continue; if (i === 4) break; s += i; } console.log(s)",
            "023",
        ),
        (
            "while and do",
            "let n = 0; while (n < 3) n++; let m = 10; do { m++; } while (m < 5); console.log(n, m)",
            "3 11",
        ),
        (
            "labels",
            "let out = []; outer: for (let i = 0; i < 3; i++) { for (let j = 0; j < 3; j++) { if (j === 1) continue outer; if (i === 2) break outer; out.push(i + '' + j); } } console.log(out.join())",
            "00,10",
        ),
        (
            "labeled block",
            "block: { console.log('in'); break block; console.log('never'); } console.log('out')",
            "in\nout",
        ),
        (
            "switch",
            "function f(x) { switch (x) { case 1: return 'one'; case 2: case 3: return 'few'; default: return 'many'; } } console.log(f(1), f(3), f(9))",
            "one few many",
        ),
        (
            "switch fallthrough",
            "let s = ''; switch (2) { case 1: s += 'a'; case 2: s += 'b'; case 3: s += 'c'; break; case 4: s += 'd'; } console.log(s)",
            "bc",
        ),
        (
            "for in",
            "const o = { b: 1, a: 2, 1: 'x', 0: 'y' }; const keys = []; for (const k in o) keys.push(k); console.log(keys.join())",
            "0,1,b,a",
        ),
        (
            "for in inherited",
            "const base = { inherited: 1 }; const o = Object.create(base); o.own = 2; const keys = []; for (const k in o) keys.push(k); console.log(keys.join())",
            "own,inherited",
        ),
        (
            "for of",
            "let total = 0; for (const n of [1, 2, 3]) total += n; for (const c of 'ab') total += c; console.log(total)",
            "6ab",
        ),
        (
            "closures per iteration",
            "const fs = []; for (let i = 0; i < 3; i++) fs.push(() => i); console.log(fs.map((f) => f()).join())",
            "0,1,2",
        ),
        (
            "var hoisting",
            "function f() { console.log(v); var v = 1; return v; } console.log(f())",
            "undefined\n1",
        ),
        (
            "function hoisting",
            "console.log(hoisted()); function hoisted() { return 'up'; }",
            "up",
        ),
        (
            "try finally",
            "function f() { try { return 'try'; } finally { console.log('finally'); } } console.log(f())",
            "finally\ntry",
        ),
        (
            "finally overrides",
            "function f() { try { throw new Error('x'); } catch (e) { return 'catch'; } finally { return 'finally'; } } console.log(f())",
            "finally",
        ),
        (
            "catch without binding",
            "try { null.x; } catch { console.log('caught'); }",
            "caught",
        ),
        (
            "break through finally",
            "let log = []; for (const i of [1, 2]) { try { if (i === 1) continue; break; } finally { log.push('f' + i); } } console.log(log.join())",
            "f1,f2",
        ),
        (
            "nested rethrow",
            "try { try { throw 1; } finally { console.log('inner'); } } catch (e) { console.log('outer', e); }",
            "inner\nouter 1",
        ),
        (
            "throw any value",
            "try { throw { code: 7 }; } catch (e) { console.log(e.code); }",
            "7",
        ),
        (
            "TDZ",
            "try { x; let x = 1; } catch (e) { console.log(e instanceof ReferenceError); }",
            "true",
        ),
        (
            "const assignment",
            "const c = 1; try { c = 2; } catch (e) { console.log(e.name); }",
            "TypeError",
        ),
        (
            "block scoping",
            "let a = 1; { let a = 2; console.log(a); } console.log(a)",
            "2\n1",
        ),
    ]);
}

#[test]
fn functions_and_destructuring_work() {
    check(&[
        (
            "defaults",
            "function f(a, b = a * 2, ...rest) { return [a, b, rest.length].join(); } console.log(f(1), f(1, 5, 6, 7), f.length)",
            "1,2,0 1,5,2 1",
        ),
        (
            "arguments",
            "function f() { return arguments.length + ':' + arguments[1]; } console.log(f('a', 'b', 'c'))",
            "3:b",
        ),
        (
            "spread calls",
            "function sum(a, b, c) { return a + b + c; } const args = [1, 2]; console.log(sum(...args, 3), sum(...[1, 2, 3]))",
            "6 6",
        ),
        (
            "object destructuring",
            "const { a, b: { c = 3 } = {}, ...rest } = { a: 1, d: 4, e: 5 }; console.log(a, c, Object.keys(rest).join())",
            "1 3 d,e",
        ),
        (
            "array destructuring",
            "const [x, , y = 9, ...z] = [1, 2, undefined, 4, 5]; console.log(x, y, z.join())",
            "1 9 4,5",
        ),
        (
            "swap",
            "let a = 1, b = 2; [a, b] = [b, a]; console.log(a, b)",
            "2 1",
        ),
        (
            "parameter patterns",
            "function f({ name, tags: [first] = [] }) { return name + first; } console.log(f({ name: 'n', tags: ['t'] }))",
            "nt",
        ),
        (
            "arrow this",
            "const o = { name: 'o', run() { return [1].map(() => this.name)[0]; } }; console.log(o.run())",
            "o",
        ),
        (
            "method this",
            "const o = { v: 3, get() { return this.v; } }; const g = o.get; console.log(o.get(), (0, o.get).call({ v: 4 }))",
            "3 4",
        ),
        (
            "call apply bind",
            "function f(a, b) { return this.x + a + b; } const t = { x: 1 }; console.log(f.call(t, 2, 3), f.apply(t, [4, 5]), f.bind(t, 10)(20), f.bind(t).name)",
            "6 10 31 bound f",
        ),
        (
            "recursion",
            "function fib(n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); } console.log(fib(20))",
            "6765",
        ),
        (
            "deep recursion",
            "function depth(n) { return n === 0 ? 0 : 1 + depth(n - 1); } console.log(depth(5000))",
            "5000",
        ),
        (
            "named function expression",
            "const f = function fact(n) { return n <= 1 ? 1 : n * fact(n - 1); }; console.log(f(5), f.name)",
            "120 fact",
        ),
        (
            "inferred names",
            "const a = () => 1; const o = { m() {}, k: function () {} }; console.log(a.name, o.m.name, o.k.name)",
            "a m k",
        ),
        (
            "function constructor",
            "const add = new Function('a', 'b', 'return a + b'); console.log(add(2, 3), add.length)",
            "5 2",
        ),
        (
            "tagged template",
            "function tag(strings, ...values) { return strings.raw.join('|') + values.join(','); } console.log(tag`a${1}b${2}\\n`)",
            "a|b|\\n1,2",
        ),
        (
            "template literal",
            "const n = 3; console.log(`n=${n + 1} ${[1, 2]}`)",
            "n=4 1,2",
        ),
        (
            "getters in literals",
            "const o = { _v: 1, get v() { return this._v * 10; }, set v(x) { this._v = x; } }; o.v = 5; console.log(o.v)",
            "50",
        ),
        (
            "computed and shorthand",
            "const k = 'key', v = 1; const o = { [k + 2]: v, v, 'quoted': 2, 3: 'n' }; console.log(o.key2, o.v, o.quoted, o[3])",
            "1 1 2 n",
        ),
        (
            "proto in literal",
            "const base = { hi() { return 'hi'; } }; const o = { __proto__: base }; console.log(o.hi(), Object.getPrototypeOf(o) === base)",
            "hi true",
        ),
        (
            "spread objects",
            "const a = { x: 1, y: 2 }; const b = { ...a, y: 3, ...null }; console.log(JSON.stringify(b))",
            "{\"x\":1,\"y\":3}",
        ),
    ]);
}

#[test]
fn classes_follow_the_specification() {
    check(&[
        (
            "methods and statics",
            "class P { constructor(x) { this.x = x; } double() { return this.x * 2; } static of(x) { return new P(x); } } console.log(P.of(4).double(), typeof P)",
            "8 function",
        ),
        (
            "inheritance and super",
            "class A { constructor(n) { this.n = n; } hi() { return 'A' + this.n; } } class B extends A { constructor() { super(2); } hi() { return super.hi() + 'B'; } } console.log(new B().hi())",
            "A2B",
        ),
        (
            "fields",
            "class C { a = 1; b = this.a + 1; static s = 'static'; } const c = new C(); console.log(c.a, c.b, C.s)",
            "1 2 static",
        ),
        (
            "private members",
            "class Counter { #count = 0; #step() { this.#count++; } get value() { return this.#count; } tick() { this.#step(); return this; } static has(o) { return #count in o; } } const c = new Counter(); c.tick().tick(); console.log(c.value, Counter.has(c), Counter.has({}))",
            "2 true false",
        ),
        (
            "private brand check",
            "class A { #x = 1; static read(o) { return o.#x; } } try { A.read({}); } catch (e) { console.log(e instanceof TypeError); }",
            "true",
        ),
        (
            "static blocks",
            "class C { static v; static { C.v = 'ready'; } } console.log(C.v)",
            "ready",
        ),
        (
            "accessors",
            "class T { #t = 0; get temp() { return this.#t; } set temp(v) { this.#t = v * 2; } } const t = new T(); t.temp = 5; console.log(t.temp)",
            "10",
        ),
        (
            "new.target",
            "class A { constructor() { this.kind = new.target.name; } } class B extends A {} console.log(new A().kind, new B().kind)",
            "A B",
        ),
        (
            "extends Error",
            "class AppError extends Error { constructor(m) { super(m); this.name = 'AppError'; } } try { throw new AppError('bad'); } catch (e) { console.log(e instanceof AppError, e instanceof Error, e.message, String(e)); }",
            "true true bad AppError: bad",
        ),
        (
            "extends null prototype",
            "class A {} console.log(Object.getPrototypeOf(A.prototype) === Object.prototype)",
            "true",
        ),
        (
            "class without new",
            "class A {} try { A(); } catch (e) { console.log(e instanceof TypeError); }",
            "true",
        ),
        (
            "super before this",
            "class A {} class B extends A { constructor() { this.x = 1; super(); } } try { new B(); } catch (e) { console.log(e instanceof ReferenceError); }",
            "true",
        ),
        (
            "derived default constructor",
            "class A { constructor(a, b) { this.sum = a + b; } } class B extends A {} console.log(new B(2, 3).sum)",
            "5",
        ),
        (
            "function constructors",
            "function Point(x) { this.x = x; } Point.prototype.get = function () { return this.x; }; const p = new Point(7); console.log(p.get(), p.constructor === Point)",
            "7 true",
        ),
        (
            "class expression",
            "const K = class Named { who() { return Named.name; } }; console.log(new K().who())",
            "Named",
        ),
        (
            "symbol methods",
            "class Bag { *[Symbol.iterator]() { yield 1; yield 2; } } console.log([...new Bag()].join())",
            "1,2",
        ),
        (
            "toString override",
            "class V { toString() { return 'V!'; } } console.log(`${new V()}`, new V() + '')",
            "V! V!",
        ),
    ]);
}

#[test]
fn objects_and_builtins_behave_like_the_spec() {
    check(&[
        (
            "property order",
            "const o = { b: 1, 2: 'two', a: 2, 1: 'one' }; console.log(Object.keys(o).join())",
            "1,2,b,a",
        ),
        (
            "defineProperty",
            "const o = {}; Object.defineProperty(o, 'x', { value: 1 }); o.x = 2; console.log(o.x, Object.keys(o).length, JSON.stringify(Object.getOwnPropertyDescriptor(o, 'x')))",
            "1 0 {\"value\":1,\"writable\":false,\"enumerable\":false,\"configurable\":false}",
        ),
        (
            "strict read-only write",
            "'use strict'; const o = Object.freeze({ a: 1 }); try { o.a = 2; } catch (e) { console.log(e instanceof TypeError, Object.isFrozen(o)); }",
            "true true",
        ),
        (
            "accessor descriptors",
            "const o = {}; let hits = 0; Object.defineProperty(o, 'v', { get() { hits++; return 42; }, enumerable: true }); console.log(o.v, o.v, hits, Object.keys(o).join())",
            "42 42 2 v",
        ),
        (
            "prototype chain",
            "const a = { greet() { return 'hi ' + this.name; } }; const b = Object.create(a, { name: { value: 'b', enumerable: true } }); console.log(b.greet(), a.isPrototypeOf(b), b.hasOwnProperty('greet'))",
            "hi b true false",
        ),
        (
            "setPrototypeOf",
            "const a = { x: 1 }; const b = {}; Object.setPrototypeOf(b, a); console.log(b.x, Object.getPrototypeOf(b) === a)",
            "1 true",
        ),
        (
            "null prototype",
            "const o = Object.create(null); o.k = 1; console.log(typeof o.toString, Object.keys(o).join())",
            "undefined k",
        ),
        (
            "toString tags",
            "console.log(Object.prototype.toString.call([]), Object.prototype.toString.call(null), Object.prototype.toString.call(() => 1), String({}))",
            "[object Array] [object Null] [object Function] [object Object]",
        ),
        (
            "symbols",
            "const s = Symbol('tag'); const o = { [s]: 1, plain: 2 }; console.log(s.toString(), s.description, Object.keys(o).join(), o[s], Object.getOwnPropertySymbols(o).length)",
            "Symbol(tag) tag plain 1 1",
        ),
        (
            "symbol registry",
            "console.log(Symbol.for('app') === Symbol.for('app'), Symbol('a') === Symbol('a'), Symbol.keyFor(Symbol.for('app')))",
            "true false app",
        ),
        (
            "wrappers",
            "console.log(typeof new Number(5), new Number(5) + 1, new String('ab').length, Boolean(''), String(123), Number('12'))",
            "object 6 2 false 123 12",
        ),
        (
            "number methods",
            "console.log((255).toString(16), (0.5).toString(2), Number.isInteger(5), Number.isNaN('x'), Number.MAX_SAFE_INTEGER)",
            "ff 0.1 true false 9007199254740991",
        ),
        (
            "errors",
            "const e = new RangeError('out'); console.log(e.name, e.message, e instanceof Error, String(e), Object.prototype.toString.call(e))",
            "RangeError out true RangeError: out [object Error]",
        ),
        (
            "error cause",
            "const e = new Error('top', { cause: 'root' }); console.log(e.cause)",
            "root",
        ),
        (
            "array constructor",
            "console.log(Array(3).length, Array(1, 2).join(), Array.of(7).join(), Array.from('abc').join('-'), Array.from({ length: 2 }, (_, i) => i * 2).join())",
            "3 1,2 7 a-b-c 0,2",
        ),
        (
            "array iterators",
            "const it = ['a', 'b'].entries(); console.log([...it].map((e) => e.join(':')).join())",
            "0:a,1:b",
        ),
        (
            "JSON parse",
            "const v = JSON.parse('{\"b\":[1,{\"c\":null}],\"a\":\"\\\\u0041\"}'); console.log(Object.keys(v).join(), v.b[1].c, v.a)",
            "b,a null A",
        ),
        (
            "JSON syntax error",
            "try { JSON.parse('{bad'); } catch (e) { console.log(e.name); }",
            "SyntaxError",
        ),
        (
            "globalThis",
            "var g = 1; console.log(globalThis.g, window.g, typeof globalThis.Object)",
            "1 1 function",
        ),
        (
            "hasOwn",
            "console.log(Object.hasOwn({ a: 1 }, 'a'), Object.hasOwn({}, 'toString'))",
            "true false",
        ),
        (
            "fromEntries",
            "console.log(JSON.stringify(Object.fromEntries([['a', 1], ['b', 2]])))",
            "{\"a\":1,\"b\":2}",
        ),
        (
            "Object.is",
            "console.log(Object.is(NaN, NaN), Object.is(0, -0), Object.is('a', 'a'))",
            "true false true",
        ),
        (
            "unknown host property",
            "console.log(document.notAProperty, typeof window.alsoMissing)",
            "undefined undefined",
        ),
    ]);
}

#[test]
fn generators_iterate_lazily() {
    check(&[
        (
            "basic",
            "function* g() { yield 1; yield 2; return 3; } const it = g(); console.log(JSON.stringify([it.next(), it.next(), it.next(), it.next()]))",
            "[{\"value\":1,\"done\":false},{\"value\":2,\"done\":false},{\"value\":3,\"done\":true},{\"done\":true}]",
        ),
        (
            "send values",
            "function* g() { const a = yield 'first'; const b = yield a * 2; return a + b; } const it = g(); it.next(); console.log(it.next(5).value, it.next(7).value)",
            "10 12",
        ),
        (
            "infinite",
            "function* naturals() { let n = 0; while (true) yield n++; } const out = []; for (const n of naturals()) { if (n > 4) break; out.push(n); } console.log(out.join())",
            "0,1,2,3,4",
        ),
        (
            "delegation",
            "function* inner() { yield 'a'; yield 'b'; return 'r'; } function* outer() { const r = yield* inner(); yield r; yield* [1, 2]; } console.log([...outer()].join())",
            "a,b,r,1,2",
        ),
        (
            "return closes",
            "let closed = false; function* g() { try { yield 1; yield 2; } finally { closed = true; } } for (const v of g()) { break; } console.log(closed)",
            "true",
        ),
        (
            "throw into",
            "function* g() { try { yield 1; } catch (e) { yield 'caught ' + e; } } const it = g(); it.next(); console.log(it.throw('boom').value)",
            "caught boom",
        ),
        (
            "early return",
            "function* g() { yield 1; yield 2; } const it = g(); it.next(); console.log(JSON.stringify(it.return('done')), JSON.stringify(it.next()))",
            "{\"value\":\"done\",\"done\":true} {\"done\":true}",
        ),
        (
            "custom iterable",
            "const range = { from: 1, to: 3, [Symbol.iterator]() { let c = this.from, t = this.to; return { next: () => c <= t ? { value: c++, done: false } : { value: undefined, done: true } }; } }; console.log([...range].join(), Array.from(range).length)",
            "1,2,3 3",
        ),
        (
            "destructuring a generator",
            "function* g() { yield 'x'; yield 'y'; yield 'z'; } const [first, ...others] = g(); console.log(first, others.join())",
            "x y,z",
        ),
        (
            "generator methods",
            "class Tree { constructor(v) { this.v = v; } *[Symbol.iterator]() { yield this.v; } } const o = { *items() { yield* new Tree(9); } }; console.log([...o.items()].join())",
            "9",
        ),
    ]);
}

#[test]
fn async_functions_suspend_on_pending_promises() {
    check(&[
        (
            "ordering",
            "async function f() { console.log('start'); await null; console.log('after'); } f(); console.log('sync')",
            "start\nsync\nafter",
        ),
        (
            "await values",
            "async function f() { const a = await 1; const b = await Promise.resolve(2); return a + b; } f().then((v) => console.log(v))",
            "3",
        ),
        (
            "pending resolution",
            "let resolve; const p = new Promise((r) => { resolve = r; }); async function wait() { const v = await p; console.log('got', v); } wait(); console.log('waiting'); resolve('it')",
            "waiting\ngot it",
        ),
        (
            "rejection",
            "async function f() { throw new TypeError('nope'); } f().catch((e) => console.log(e.name, e.message))",
            "TypeError nope",
        ),
        (
            "try around await",
            "async function f() { try { await Promise.reject(new Error('r')); } catch (e) { return 'handled ' + e.message; } } f().then(console.log)",
            "handled r",
        ),
        (
            "thenable",
            "const thenable = { then(resolve) { resolve('thenable value'); } }; (async () => console.log(await thenable))()",
            "thenable value",
        ),
        (
            "async arrow",
            "const f = async (x) => x * 2; f(21).then((v) => console.log(v))",
            "42",
        ),
        (
            "async methods",
            "class Api { async load() { await 0; return 'loaded'; } } new Api().load().then(console.log)",
            "loaded",
        ),
        (
            "sequential awaits",
            "async function f() { const out = []; for (const n of [1, 2, 3]) { out.push(await n); } return out.join(); } f().then(console.log)",
            "1,2,3",
        ),
        (
            "promise chain",
            "Promise.resolve(1).then((v) => v + 1).then((v) => { throw v; }).catch((v) => console.log('caught', v))",
            "caught 2",
        ),
        (
            "returning promise",
            "async function inner() { return 'inner'; } async function outer() { return inner(); } outer().then(console.log)",
            "inner",
        ),
        (
            "finally with await",
            "async function f() { try { await null; return 'body'; } finally { console.log('cleanup'); } } f().then(console.log)",
            "cleanup\nbody",
        ),
    ]);
}

#[test]
fn async_generators_and_for_await_work() {
    check(&[
        (
            "for await",
            "async function* g() { yield 1; await null; yield 2; } (async () => { const out = []; for await (const v of g()) out.push(v); console.log(out.join()); })()",
            "1,2",
        ),
        (
            "for await over promises",
            "(async () => { let s = 0; for await (const v of [Promise.resolve(1), 2, Promise.resolve(3)]) s += v; console.log(s); })()",
            "6",
        ),
        (
            "next queue",
            "async function* g() { yield 'a'; yield 'b'; } const it = g(); it.next().then((r) => console.log(r.value)); it.next().then((r) => console.log(r.value)); it.next().then((r) => console.log(r.done))",
            "a\nb\ntrue",
        ),
        (
            "yield awaits",
            "async function* g() { yield Promise.resolve('resolved'); } g().next().then((r) => console.log(r.value))",
            "resolved",
        ),
        (
            "async delegation",
            "async function* inner() { yield 1; yield 2; } async function* outer() { yield* inner(); yield 3; } (async () => { const out = []; for await (const v of outer()) out.push(v); console.log(out.join()); })()",
            "1,2,3",
        ),
        (
            "async iterator protocol",
            "const source = { [Symbol.asyncIterator]() { let i = 0; return { next: async () => ({ value: i, done: i++ >= 2 }) }; } }; (async () => { const out = []; for await (const v of source) out.push(v); console.log(out.join()); })()",
            "0,1",
        ),
    ]);
}

#[test]
fn errors_surface_with_spec_types() {
    check(&[
        (
            "reference",
            "try { missing; } catch (e) { console.log(e.name, e.message); }",
            "ReferenceError missing is not defined",
        ),
        (
            "null property",
            "try { null.x; } catch (e) { console.log(e.name); }",
            "TypeError",
        ),
        (
            "not a function",
            "try { (1)(); } catch (e) { console.log(e.name); }",
            "TypeError",
        ),
        (
            "not iterable",
            "try { const [a] = 1; } catch (e) { console.log(e.name); }",
            "TypeError",
        ),
        (
            "stack overflow",
            "function f() { return f(); } try { f(); } catch (e) { console.log(e.name); }",
            "RangeError",
        ),
    ]);
    let error = run("throw new Error('uncaught');").expect_err("uncaught throws reach the host");
    assert_eq!(error, "uncaught");
}

#[test]
fn edge_cases_hold() {
    check(&[
        (
            "object super",
            "const base = { hi() { return 'base'; } }; const o = { __proto__: base, hi() { return super.hi() + '+'; } }; console.log(o.hi())",
            "base+",
        ),
        (
            "super property assignment",
            "class A {} class B extends A { set() { super.x = 1; super['y'] = 2; return this.x + this.y; } } console.log(new B().set())",
            "3",
        ),
        (
            "super in arrow",
            "class A { v() { return 'a'; } } class B extends A { v() { return (() => super.v())(); } } console.log(new B().v())",
            "a",
        ),
        (
            "inherited accessor receiver",
            "const proto = { get who() { return this.name; } }; const o = Object.create(proto); o.name = 'child'; console.log(o.who)",
            "child",
        ),
        (
            "inherited setter",
            "let seen; const proto = { set v(x) { seen = x; } }; const o = Object.create(proto); o.v = 9; console.log(seen, Object.keys(o).length)",
            "9 0",
        ),
        (
            "sloppy this",
            "function f() { return this === globalThis; } console.log(f())",
            "true",
        ),
        (
            "strict this",
            "function f() { 'use strict'; return this; } console.log(f())",
            "undefined",
        ),
        (
            "arrow arguments",
            "function f() { return (() => arguments.length)(); } console.log(f(1, 2))",
            "2",
        ),
        (
            "default scope",
            "function f(a, g = () => a) { a = 2; return g(); } console.log(f(1))",
            "2",
        ),
        (
            "toPrimitive",
            "const o = { [Symbol.toPrimitive](hint) { return hint === 'number' ? 42 : 'str'; } }; console.log(+o, `${o}`, o + '')",
            "42 str str",
        ),
        (
            "for of closures",
            "const fs = []; for (const x of [1, 2]) fs.push(() => x); for (const k in { a: 1, b: 2 }) fs.push(() => k); console.log(fs.map((f) => f()).join())",
            "1,2,a,b",
        ),
        (
            "iterator close on break",
            "let closed = 0; const it = { [Symbol.iterator]() { return { next: () => ({ value: 1, done: false }), return() { closed++; return {}; } }; } }; for (const v of it) break; const [a] = it; console.log(closed)",
            "2",
        ),
        (
            "finally in generator",
            "function* g() { try { yield 1; } finally { yield 'cleanup'; } } const it = g(); it.next(); console.log(it.return('x').value, it.next().value)",
            "cleanup x",
        ),
        (
            "host error in try",
            "try { document.createElement(); } catch (e) { console.log(e instanceof Error, typeof e.message); }",
            "true string",
        ),
        (
            "array holes",
            "const a = [1, , 3]; console.log(a.length, a[1], 1 in a)",
            "3 undefined true",
        ),
        (
            "array length",
            "const a = [1, 2, 3]; a.length = 1; console.log(a.join(), a.length)",
            "1 1",
        ),
        (
            "array expandos",
            "const a = [1]; a.label = 'x'; console.log(a.label, a.length)",
            "x 1",
        ),
        (
            "string index",
            "const s = 'héllo'; console.log(s[1], s.length, s['0'])",
            "é 5 h",
        ),
        (
            "exceptions in getters",
            "const o = { get bad() { throw new Error('getter'); } }; try { o.bad; } catch (e) { console.log(e.message); }",
            "getter",
        ),
        (
            "nested functions see globals",
            "var counter = 0; function bump() { counter++; } bump(); bump(); console.log(counter, window.counter)",
            "2 2",
        ),
        (
            "implicit globals",
            "function leak() { leaked = 5; } leak(); console.log(leaked)",
            "5",
        ),
        (
            "strict undeclared",
            "'use strict'; try { undeclaredVariable = 1; } catch (e) { console.log(e.name); }",
            "ReferenceError",
        ),
        (
            "let shadow in switch",
            "switch (1) { case 1: { let x = 'inner'; console.log(x); } }",
            "inner",
        ),
        (
            "async throw before await",
            "async function f() { throw 'early'; } let caught = false; f().catch(() => { caught = true; console.log('caught'); }); console.log('after', caught)",
            "after false
caught",
        ),
        (
            "promise resolve with promise",
            "const p = Promise.resolve(Promise.resolve('inner')); p.then(console.log)",
            "inner",
        ),
        (
            "then ignores non functions",
            "Promise.resolve(5).then(null).then(undefined, 1).then((v) => console.log(v))",
            "5",
        ),
        (
            "await in loop condition",
            "async function f() { let i = 0; while (await (i < 3)) i++; return i; } f().then(console.log)",
            "3",
        ),
        (
            "generator this",
            "const o = { *g() { yield this.v; }, v: 'ok' }; console.log(o.g().next().value)",
            "ok",
        ),
        (
            "getter setter pairs in classes",
            "class A { static #n = 0; static get n() { return A.#n; } static set n(v) { A.#n = v; } } A.n = 4; console.log(A.n)",
            "4",
        ),
        (
            "object spread getters",
            "const src = { get x() { return 'computed'; } }; const copy = { ...src }; console.log(Object.getOwnPropertyDescriptor(copy, 'x').value)",
            "computed",
        ),
        (
            "json roundtrip",
            "const v = JSON.parse(JSON.stringify({ a: [1, 'two', null, true], b: { c: 1.5 } })); console.log(v.a[1], v.b.c, v.a.length)",
            "two 1.5 4",
        ),
        (
            "console formatting",
            "console.log([1, [2, 3]], 1.50, -0, null, undefined, true)",
            "1,2,3 1.5 0 null undefined true",
        ),
    ]);
}
