use ceri_engine::{Document, ScriptRuntime};
use std::cell::RefCell;
use std::rc::Rc;

fn run(source: &str) -> Result<String, String> {
    let document = Document::parse("<p id='status'></p>").expect("document");
    let mut runtime = ScriptRuntime::new(Rc::new(RefCell::new(document)));
    runtime
        .execute(source)
        .map_err(|error| error.message.clone())?;
    runtime
        .advance_time(1000)
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
            "0.30000000000000004 1e+21 123456789012345680000 0.3333333333333333 -0 9007199254740992 5e-7",
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
            "not iterable names the source as v8 does",
            "var o = { list: null, f() {} }, e = null, a, said = [];
            for (const run of [() => [...e], () => { for (const x of o.f()); }, () => { const [a] = e; },
                () => { const [a] = o.f; }, () => { [a] = e; }, () => [...[1].q], () => [...(0, !e)],
                () => [...(o.list ?? -1)], () => [...o?.list], () => [...e ? e : e], () => Math.max(...o.list),
                () => Math.max(...1), () => new Set(1), () => [...{}], () => [...o['list']], () => [...o[e]],
                () => [...~1], () => [...!0], () => [...1 + 2], () => [...o['li' + 'st']], () => { const [a] = -1; },
                () => { const [a] = 's'.length; }, () => { const [[a]] = [1]; }, () => [...(e, e, 1)], () => [...{ a: 1, b: e }]]) {
                try { run(); } catch (error) { said.push(error.message); }
            }
            console.log(said.join('|'));",
            "e is not iterable|o.f is not a function or its return value is not iterable|e is not iterable|function is not iterable (cannot read property Symbol(Symbol.iterator))|object null is not iterable (cannot read property Symbol(Symbol.iterator))|[1].q is not iterable|(0 , (!e)) is not iterable|(o.list ?? -1) is not iterable|(intermediate value) is not iterable|(intermediate value)(intermediate value)(intermediate value) is not iterable|o.list is not iterable (cannot read property null)|Spread syntax requires ...iterable[Symbol.iterator] to be a function|number 1 is not iterable (cannot read property Symbol(Symbol.iterator))|{} is not iterable|o.list is not iterable|o[e] is not iterable|-2 is not iterable|true is not iterable|3 is not iterable|o[\"list\"] is not iterable|-1 is not iterable|number 1 is not iterable (cannot read property Symbol(Symbol.iterator))|number 1 is not iterable (cannot read property Symbol(Symbol.iterator))|(e , e , 1) is not iterable|{(intermediate value)(intermediate value)} is not iterable",
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
            "[ 1, [ 2, 3 ] ] 1.5 -0 null undefined true",
        ),
    ]);
}

#[test]
fn array_methods_follow_the_specification() {
    check(&[
        (
            "iteration",
            "const a = [3, 1, 2]; console.log(a.map((x) => x * 2).join(), a.filter((x) => x > 1).length, a.some((x) => x > 2), a.every((x) => x > 0), a.find((x) => x < 3), a.findIndex((x) => x === 2), a.findLast((x) => x > 0), a.findLastIndex((x) => x > 5))",
            "6,2,4 2 true true 1 2 2 -1",
        ),
        (
            "reduce",
            "console.log([1, 2, 3].reduce((s, x) => s + x), [1, 2, 3].reduce((s, x) => s + x, 10), ['a', 'b'].reduceRight((s, x) => s + x))",
            "6 16 ba",
        ),
        (
            "searching",
            "const a = [1, 2, NaN, 2]; console.log(a.indexOf(2), a.lastIndexOf(2), a.includes(NaN), a.indexOf(NaN), a.at(-1), [].at(0))",
            "1 3 true -1 2 undefined",
        ),
        (
            "slicing",
            "const a = [1, 2, 3, 4, 5]; console.log(a.slice(1, -1).join(), a.slice(-2).join(), a.concat([6], 7).length)",
            "2,3,4 4,5 7",
        ),
        (
            "splice",
            "const a = [1, 2, 3, 4]; const removed = a.splice(1, 2, 'x', 'y', 'z'); console.log(a.join(), removed.join(), a.toSpliced(0, 1).join(), a.length)",
            "1,x,y,z,4 2,3 x,y,z,4 5",
        ),
        (
            "stack and queue",
            "const a = [1]; a.push(2, 3); a.unshift(0); console.log(a.pop(), a.shift(), a.join())",
            "3 0 1,2",
        ),
        (
            "sort",
            "console.log([10, 9, 1, 100].sort().join(), [10, 9, 1, 100].sort((a, b) => a - b).join(), ['b', undefined, 'a'].sort().join())",
            "1,10,100,9 1,9,10,100 a,b,",
        ),
        (
            "stable sort",
            "const items = [{k: 1, v: 'a'}, {k: 0, v: 'b'}, {k: 1, v: 'c'}, {k: 0, v: 'd'}]; console.log(items.sort((x, y) => x.k - y.k).map((i) => i.v).join(''))",
            "bdac",
        ),
        (
            "inconsistent comparator",
            "const a = Array.from({ length: 50 }, (_, i) => i); a.sort(() => 1); console.log(a.length)",
            "50",
        ),
        (
            "reverse and fill",
            "console.log([1, 2, 3].reverse().join(), new Array(3).fill(0).join(), [1, 2, 3, 4].fill(9, 1, 3).join(), [1, 2, 3].toReversed().join())",
            "3,2,1 0,0,0 1,9,9,4 3,2,1",
        ),
        (
            "flat",
            "console.log([1, [2, [3, [4]]]].flat().length, [1, [2, [3, [4]]]].flat(Infinity).join(), [1, 2].flatMap((x) => [x, x * 10]).join())",
            "3 1,2,3,4 1,10,2,20",
        ),
        (
            "copyWithin and with",
            "console.log([1, 2, 3, 4, 5].copyWithin(0, 3).join(), [1, 2, 3].with(1, 'b').join())",
            "4,5,3,4,5 1,b,3",
        ),
        (
            "array-likes",
            "function f() { return Array.prototype.slice.call(arguments, 1).join('-'); } console.log(f(1, 2, 3), Array.prototype.map.call('abc', (c) => c.toUpperCase()).join(''))",
            "2-3 ABC",
        ),
        (
            "iteration helpers",
            "console.log([...['a', 'b'].keys()].join(), [...['a', 'b'].values()].join(), Array.isArray([]), Array.isArray({ length: 0 }))",
            "0,1 a,b true false",
        ),
        (
            "join cycles",
            "const a = [1]; a.push(a); console.log(a.join('-'), String([null, undefined, 1]))",
            "1- ,,1",
        ),
    ]);
}

#[test]
fn string_methods_follow_the_specification() {
    check(&[
        (
            "utf16 length",
            "const s = 'a\u{1F600}b'; console.log(s.length, s.charCodeAt(1), s.codePointAt(1), s.at(-1), [...s].length)",
            "4 55357 128512 b 3",
        ),
        (
            "slicing",
            "const s = 'Hello, world'; console.log(s.slice(-5), s.substring(5, 0), s.substr(7, 3), s.slice(3, 1) === '')",
            "world Hello wor true",
        ),
        (
            "searching",
            "const s = 'banana'; console.log(s.indexOf('an'), s.lastIndexOf('an'), s.includes('nan'), s.startsWith('ban'), s.endsWith('na'), s.indexOf(''), s.search(/n./))",
            "1 3 true true true 0 2",
        ),
        (
            "padding and repeat",
            "console.log('5'.padStart(3, '0'), 'ab'.padEnd(5, 'xy'), 'ha'.repeat(3), '|' + '  x '.trim() + '|' + ' y'.trimStart() + '|' + 'z '.trimEnd() + '|')",
            "005 abxyx hahaha |x|y|z|",
        ),
        (
            "normalization forms",
            "const e = 'caf\\u00e9'; console.log(e.normalize('NFD').length, e.normalize('NFD').normalize() === e, '\\ufb01'.normalize('NFKC'), '\\ufb01'.normalize('NFC').length, '\\u1e0b\\u0323'.normalize().codePointAt(0).toString(16)); try { e.normalize('nfc'); } catch (error) { console.log(error.name); }",
            "5 true fi 1 1e0d\nRangeError",
        ),
        (
            "case",
            "console.log('Straße'.toUpperCase(), 'ÀB'.toLowerCase(), 'i'.localeCompare('I'), 'a'.localeCompare('b'))",
            "STRASSE àb -1 -1",
        ),
        (
            "split",
            "console.log('a,b,,c'.split(',').length, 'abc'.split('').join('|'), 'a1b2c3'.split(/\\d/).join(), 'a, b ,c'.split(/\\s*,\\s*/).join('|'), 'one two'.split(' ', 1).join())",
            "4 a|b|c a,b,c, a|b|c one",
        ),
        (
            "replace strings",
            "console.log('aaa'.replace('a', 'b'), 'aaa'.replaceAll('a', 'b'), 'x-y'.replace('-', '$&$&'), 'abc'.replace('b', (m, i) => m.toUpperCase() + i))",
            "baa bbb x--y aB1c",
        ),
        (
            "replace patterns",
            "console.log('2024-01-15'.replace(/(\\d+)-(\\d+)-(\\d+)/, '$3/$2/$1'), 'a.b.c'.replace(/\\./g, '/'), 'John Smith'.replace(/(?<first>\\w+) (?<last>\\w+)/, '$<last>, $<first>'))",
            "15/01/2024 a/b/c Smith, John",
        ),
        (
            "replace callbacks",
            "console.log('the cat sat'.replace(/\\b(\\w)(\\w*)/g, (m, a, b) => a.toUpperCase() + b))",
            "The Cat Sat",
        ),
        (
            "match",
            "console.log('a1b22c333'.match(/\\d+/g).join(), 'xyz'.match(/q/), 'abc'.match(/(b)(c)/).index, 'k=v'.match(/(?<key>\\w)=(?<value>\\w)/).groups.value)",
            "1,22,333 null 1 v",
        ),
        (
            "matchAll",
            "const found = [...'a1b2'.matchAll(/[a-z](\\d)/g)].map((m) => m[1] + '@' + m.index); console.log(found.join())",
            "1@0,2@2",
        ),
        (
            "raw and fromCodePoint",
            "console.log(String.raw`a\\nb${1}`, String.fromCodePoint(128512) === '\u{1F600}', String.fromCharCode(72, 105))",
            "a\\nb1 true Hi",
        ),
        (
            "string iteration",
            "let n = 0; for (const ch of 'h\u{1F600}') n++; console.log(n)",
            "2",
        ),
    ]);
}

#[test]
fn regular_expressions_work() {
    check(&[
        (
            "test and exec",
            "const r = /(\\w+)@(\\w+)\\.com/; const m = r.exec('mail me@site.com now'); console.log(r.test('x@y.com'), m[0], m[1], m.index, m.input.length)",
            "true me@site.com me 5 20",
        ),
        (
            "global lastIndex",
            "const r = /o/g; const s = 'foo boo'; const seen = []; let m; while ((m = r.exec(s))) seen.push(m.index + ':' + r.lastIndex); console.log(seen.join(), r.lastIndex)",
            "1:2,2:3,5:6,6:7 0",
        ),
        (
            "sticky",
            "const r = /a/y; console.log(r.test('ba'), r.lastIndex, (r.lastIndex = 1, r.test('ba')), r.lastIndex)",
            "false 0 true 2",
        ),
        (
            "flags and source",
            "const r = new RegExp('a/b', 'gi'); console.log(r.source, r.flags, r.global, r.ignoreCase, r.multiline, String(r), /x/.sticky)",
            "a\\/b gi true true false /a\\/b/gi false",
        ),
        (
            "constructor from regexp",
            "const a = /x/g; const b = new RegExp(a, 'i'); console.log(b.flags, RegExp(a) === a, b instanceof RegExp)",
            "i true true",
        ),
        (
            "indices flag",
            "const m = /b(c)/d.exec('abc'); console.log(m.indices[0].join(), m.indices[1].join())",
            "1,3 2,3",
        ),
        (
            "unicode",
            "console.log(/^.$/u.test('\u{1F600}'), /^.$/.test('\u{1F600}'), /\\p{Lu}/u.test('É'), '\u{1F600}x'.match(/./gu).length)",
            "true false true 2",
        ),
        (
            "lookbehind",
            "console.log('$4 €5 $6'.match(/(?<=\\$)\\d/g).join())",
            "4,6",
        ),
        (
            "syntax errors",
            "try { new RegExp('('); } catch (e) { console.log(e.name); } try { new RegExp('a', 'gg'); } catch (e) { console.log(e.name); }",
            "SyntaxError\nSyntaxError",
        ),
        (
            "literal in loop",
            "let count = 0; for (let i = 0; i < 100; i++) { if (/^\\d+$/.test(String(i))) count++; } console.log(count)",
            "100",
        ),
        (
            "split with captures",
            "console.log('a1b2c'.split(/(\\d)/).join('|'))",
            "a|1|b|2|c",
        ),
        (
            "empty matches",
            "console.log('abc'.replace(/x*/g, '-'), 'abc'.split(/(?:)/u).join())",
            "-a-b-c- a,b,c",
        ),
    ]);
}

#[test]
fn numbers_math_and_json_work() {
    check(&[
        (
            "toFixed",
            "console.log((1.005).toFixed(2), (2.5).toFixed(0), (1234.5678).toFixed(1), (0).toFixed(2), (-1.5).toFixed(0), (1e21).toFixed(2))",
            "1.00 3 1234.6 0.00 -2 1e+21",
        ),
        (
            "toPrecision and toExponential",
            "console.log((123.456).toPrecision(4), (0.00001234).toPrecision(2), (123456).toExponential(2), (255).toString(2), (0.5).toString(16))",
            "123.5 0.000012 1.23e+5 11111111 0.8",
        ),
        (
            "toLocaleString",
            "console.log((1234567.891).toLocaleString(), (0.5).toLocaleString())",
            "1,234,567.891 0.5",
        ),
        (
            "parsing",
            "console.log(parseInt('42px'), parseInt('0x1A'), parseInt('101', 2), parseFloat('3.14abc'), parseInt(''), isNaN('abc'), isFinite('12'), Number.parseFloat === parseFloat)",
            "42 26 5 3.14 NaN true true true",
        ),
        (
            "math",
            "console.log(Math.max(1, 5, 3), Math.min(), Math.round(-2.5), Math.round(2.5), Math.sign(-3), Math.trunc(-4.7), Math.hypot(3, 4), Math.cbrt(27), Math.clz32(1), Math.imul(3, 4))",
            "5 Infinity -2 3 -1 -4 5 3 31 12",
        ),
        (
            "random range",
            "let ok = true; for (let i = 0; i < 1000; i++) { const r = Math.random(); if (r < 0 || r >= 1) ok = false; } console.log(ok, Math.PI.toFixed(5))",
            "true 3.14159",
        ),
        (
            "stringify",
            "console.log(JSON.stringify({ a: [1, 'x', null, undefined, () => 1], b: undefined, c: true, d: NaN }), JSON.stringify('he said \"hi\"\\n'), JSON.stringify(undefined))",
            "{\"a\":[1,\"x\",null,null,null],\"c\":true,\"d\":null} \"he said \\\"hi\\\"\\n\" undefined",
        ),
        (
            "stringify indentation",
            "console.log(JSON.stringify({ a: 1, b: [1, 2] }, null, 2))",
            "{\n  \"a\": 1,\n  \"b\": [\n    1,\n    2\n  ]\n}",
        ),
        (
            "stringify replacer",
            "console.log(JSON.stringify({ a: 1, b: 2, c: { a: 3 } }, ['a', 'c']), JSON.stringify({ a: 1, b: 'x' }, (k, v) => typeof v === 'number' ? v * 10 : v))",
            "{\"a\":1,\"c\":{\"a\":3}} {\"a\":10,\"b\":\"x\"}",
        ),
        (
            "toJSON",
            "const o = { toJSON(key) { return 'custom:' + key; } }; console.log(JSON.stringify({ o }), JSON.stringify([o]))",
            "{\"o\":\"custom:o\"} [\"custom:0\"]",
        ),
        (
            "circular",
            "const o = {}; o.self = o; try { JSON.stringify(o); } catch (e) { console.log(e.name); }",
            "TypeError",
        ),
        (
            "reviver",
            "const v = JSON.parse('{\"a\":1,\"b\":{\"c\":2}}', (k, v) => typeof v === 'number' ? v + 1 : v); console.log(v.a, v.b.c)",
            "2 3",
        ),
    ]);
}

#[test]
fn console_promises_and_timers_work() {
    check(&[
        (
            "console inspection",
            "console.log({ a: 1, b: 'two', c: [1, { d: null }] }, 'plain', [function named() {}], new Error('boom'))",
            "{ a: 1, b: 'two', c: [ 1, { d: null } ] } plain [ [Function: named] ] Error: boom",
        ),
        (
            "console format",
            "console.log('%s has %d items costing %f', 'cart', 3.7, '2.5', 'extra')",
            "cart has 3 items costing 2.5 extra",
        ),
        (
            "console groups and counts",
            "console.group('outer'); console.info('inside'); console.groupEnd(); console.count(); console.count(); console.warn('w'); console.error('e')",
            "outer\n  inside\ndefault: 1\ndefault: 2\nw\ne",
        ),
        (
            "console assert",
            "console.assert(1 === 2, 'math is broken'); console.assert(true, 'never')",
            "Assertion failed: math is broken",
        ),
        (
            "promise all",
            "Promise.all([1, Promise.resolve(2), new Promise((r) => setTimeout(() => r(3), 5))]).then((v) => console.log(v.join()))",
            "1,2,3",
        ),
        (
            "promise all rejects",
            "Promise.all([Promise.resolve(1), Promise.reject(new Error('no'))]).catch((e) => console.log(e.message))",
            "no",
        ),
        (
            "allSettled",
            "Promise.allSettled([Promise.resolve(1), Promise.reject('x')]).then((r) => console.log(r.map((s) => s.status + ':' + (s.value ?? s.reason)).join()))",
            "fulfilled:1,rejected:x",
        ),
        (
            "race and any",
            "Promise.race([new Promise((r) => setTimeout(() => r('slow'), 10)), Promise.resolve('fast')]).then(console.log); Promise.any([Promise.reject(1), Promise.resolve('any')]).then(console.log); Promise.any([Promise.reject(1)]).catch((e) => console.log(e.name, e.errors.length))",
            "fast\nany\nAggregateError 1",
        ),
        (
            "finally",
            "Promise.resolve('v').finally(() => console.log('cleanup')).then(console.log); Promise.reject(new Error('r')).finally(() => {}).catch((e) => console.log(e.message))",
            "cleanup\nv\nr",
        ),
        (
            "withResolvers",
            "const { promise, resolve } = Promise.withResolvers(); promise.then(console.log); resolve('done')",
            "done",
        ),
        (
            "queueMicrotask ordering",
            "setTimeout(() => console.log('timeout'), 0); queueMicrotask(() => console.log('micro')); Promise.resolve().then(() => console.log('promise')); console.log('sync')",
            "sync\nmicro\npromise\ntimeout",
        ),
    ]);
}

#[test]
fn collections_follow_the_specification() {
    check(&[
        (
            "map basics",
            "const m = new Map([['a', 1], [NaN, 'nan'], [0, 'zero']]); m.set('b', 2).set('a', 3); console.log(m.size, m.get('a'), m.get(NaN), m.get(-0), m.has('c'), [...m.keys()].join(), m.delete('b'), m.size)",
            "4 3 nan zero false a,NaN,0,b true 3",
        ),
        (
            "map identity keys",
            "const k1 = {}, k2 = {}; const m = new Map(); m.set(k1, 'one').set(k2, 'two'); console.log(m.get(k1), m.get(k2), m.get({}))",
            "one two undefined",
        ),
        (
            "live iteration",
            "const m = new Map([[1, 'a']]); const seen = []; for (const [k] of m) { seen.push(k); if (k < 3) m.set(k + 1, 'x'); } console.log(seen.join())",
            "1,2,3",
        ),
        (
            "delete during iteration",
            "const s = new Set([1, 2, 3, 4]); const seen = []; for (const v of s) { seen.push(v); s.delete(v + 1); } console.log(seen.join(), s.size)",
            "1,3 2",
        ),
        (
            "set operations",
            "const a = new Set([1, 2, 3]), b = new Set([2, 3, 4]); console.log([...a.union(b)].join(), [...a.intersection(b)].join(), [...a.difference(b)].join(), [...a.symmetricDifference(b)].join(), a.isSubsetOf(new Set([1, 2, 3, 4])), a.isDisjointFrom(new Set([9])))",
            "1,2,3,4 2,3 1 1,4 true true",
        ),
        (
            "forEach and entries",
            "const out = []; new Map([['x', 1]]).forEach((v, k, map) => out.push(k + v + map.size)); new Set(['y']).forEach((v, k) => out.push(v + k)); console.log(out.join(), JSON.stringify([...new Set(['a']).entries()]))",
            "x11,yy [[\"a\",\"a\"]]",
        ),
        (
            "subclass",
            "class Counter extends Map { inc(k) { this.set(k, (this.get(k) || 0) + 1); return this; } } const c = new Counter(); c.inc('a').inc('a'); console.log(c.get('a'), c instanceof Map, c instanceof Counter)",
            "2 true true",
        ),
        (
            "weak collections",
            "const wm = new WeakMap(); const key = {}; wm.set(key, 'v'); const ws = new WeakSet([key]); console.log(wm.get(key), wm.has({}), ws.has(key)); try { wm.set(1, 2); } catch (e) { console.log(e.name); }",
            "v false true\nTypeError",
        ),
        (
            "weak ref",
            "const target = { n: 1 }; const ref = new WeakRef(target); console.log(ref.deref().n)",
            "1",
        ),
        (
            "groupBy",
            "const g = Object.groupBy([1, 2, 3, 4], (n) => n % 2 ? 'odd' : 'even'); const m = Map.groupBy(['a', 'bb', 'c'], (s) => s.length); console.log(g.odd.join(), g.even.join(), m.get(1).join())",
            "1,3 2,4 a,c",
        ),
        (
            "structured clone",
            "const copy = structuredClone(new Map([['k', new Set([1])]])); console.log(copy instanceof Map, copy.get('k').has(1))",
            "true true",
        ),
        (
            "inspection",
            "console.log(new Map([['a', 1]]), new Set([1, 'x']))",
            "Map(1) { 'a' => 1 } Set(2) { 1, 'x' }",
        ),
    ]);
}

#[test]
fn dates_follow_the_specification() {
    check(&[
        (
            "utc fields",
            "const d = new Date(Date.UTC(2024, 0, 15, 10, 30, 45, 123)); console.log(d.getUTCFullYear(), d.getUTCMonth(), d.getUTCDate(), d.getUTCDay(), d.getUTCHours(), d.getUTCMinutes(), d.getUTCSeconds(), d.getUTCMilliseconds())",
            "2024 0 15 1 10 30 45 123",
        ),
        (
            "iso",
            "const d = new Date('2024-02-29T12:00:00Z'); console.log(d.toISOString(), d.getTime(), JSON.stringify({ d }))",
            "2024-02-29T12:00:00.000Z 1709208000000 {\"d\":\"2024-02-29T12:00:00.000Z\"}",
        ),
        (
            "local is utc without a zone",
            "const d = new Date(2024, 0, 15, 8, 5); console.log(d.toString(), d.getTimezoneOffset(), d.getHours())",
            "Mon Jan 15 2024 08:05:00 GMT+0000 (Coordinated Universal Time) 0 8",
        ),
        (
            "setters roll over",
            "const d = new Date(Date.UTC(2024, 0, 31)); d.setUTCMonth(1); console.log(d.toISOString()); d.setUTCDate(0); console.log(d.toISOString()); d.setUTCHours(48); console.log(d.toISOString())",
            "2024-03-02T00:00:00.000Z\n2024-02-29T00:00:00.000Z\n2024-03-02T00:00:00.000Z",
        ),
        (
            "parsing",
            "console.log(Date.parse('2024-01-15'), Date.parse('Mon, 15 Jan 2024 00:00:00 GMT'), isNaN(Date.parse('garbage')), new Date('2024-01-15T10:00:00+02:00').toISOString())",
            "1705276800000 1705276800000 true 2024-01-15T08:00:00.000Z",
        ),
        (
            "invalid dates",
            "const d = new Date('nope'); console.log(String(d), isNaN(d), d.getFullYear()); try { d.toISOString(); } catch (e) { console.log(e.name); }",
            "Invalid Date true NaN\nRangeError",
        ),
        (
            "arithmetic and comparison",
            "const a = new Date(1000), b = new Date(3000); console.log(b - a, a < b, +a, `${new Date(0).toUTCString()}`)",
            "2000 true 1000 Thu, 01 Jan 1970 00:00:00 GMT",
        ),
        (
            "locale strings",
            "const d = new Date(Date.UTC(2024, 6, 4, 15, 4, 5)); console.log(d.toLocaleDateString(), d.toLocaleTimeString(), d.toLocaleString())",
            "7/4/2024 3:04:05 PM 7/4/2024, 3:04:05 PM",
        ),
        (
            "now moves with the clock",
            "const start = Date.now(); setTimeout(() => console.log(Date.now() - start >= 50), 50)",
            "true",
        ),
        (
            "two-digit years",
            "console.log(new Date(99, 0).getFullYear(), new Date(Date.UTC(2024, 0)).getUTCFullYear())",
            "1999 2024",
        ),
    ]);
}

#[test]
fn proxies_and_reflect_work() {
    check(&[
        (
            "get trap",
            "const p = new Proxy({ a: 1 }, { get(t, k, r) { return k in t ? t[k] * 10 : 'missing:' + String(k); } }); console.log(p.a, p.b)",
            "10 missing:b",
        ),
        (
            "set and has traps",
            "const log = []; const p = new Proxy({}, { set(t, k, v) { log.push(k + '=' + v); t[k] = v; return true; }, has(t, k) { return k.startsWith('x'); } }); p.x = 1; p.y = 2; console.log(log.join(), 'xyz' in p, 'abc' in p)",
            "x=1,y=2 true false",
        ),
        (
            "ownKeys",
            "const p = new Proxy({ a: 1, b: 2 }, { ownKeys(t) { return ['b', 'a']; } }); console.log(Object.keys(p).join(), JSON.stringify(p))",
            "b,a {\"b\":2,\"a\":1}",
        ),
        (
            "deleteProperty",
            "const t = { a: 1 }; const p = new Proxy(t, { deleteProperty(target, k) { delete target[k]; return true; } }); delete p.a; console.log('a' in t)",
            "false",
        ),
        (
            "function proxies",
            "const f = new Proxy(function (a, b) { return a + b; }, { apply(t, thisArg, args) { return t(...args) * 2; } }); console.log(f(1, 2), typeof f)",
            "6 function",
        ),
        (
            "construct trap",
            "class A { constructor(x) { this.x = x; } } const P = new Proxy(A, { construct(t, args) { return new t(args[0] + 1); } }); console.log(new P(1).x)",
            "2",
        ),
        (
            "forwarding",
            "const target = { greet() { return 'hi ' + this.name; }, name: 'target' }; const p = new Proxy(target, {}); p.extra = 1; console.log(p.greet(), target.extra, Object.getPrototypeOf(p) === Object.prototype)",
            "hi target 1 true",
        ),
        (
            "reactive pattern",
            "const deps = []; function reactive(o) { return new Proxy(o, { get(t, k, r) { deps.push(k); return Reflect.get(t, k, r); }, set(t, k, v, r) { const ok = Reflect.set(t, k, v, r); deps.push('set:' + k); return ok; } }); } const state = reactive({ count: 0 }); state.count++; console.log(state.count, deps.join())",
            "1 count,set:count,count",
        ),
        (
            "revocable",
            "const { proxy, revoke } = Proxy.revocable({}, {}); proxy.a = 1; revoke(); try { proxy.a; } catch (e) { console.log(e.name); }",
            "TypeError",
        ),
        (
            "reflect",
            "const o = {}; Reflect.defineProperty(o, 'x', { value: 1, enumerable: true }); console.log(Reflect.has(o, 'x'), Reflect.ownKeys({ a: 1, [Symbol.iterator]: 2 }).length, Reflect.getPrototypeOf([]) === Array.prototype, Reflect.apply(Math.max, null, [1, 3, 2]), Reflect.construct(Date, [0]).getTime())",
            "true 2 true 3 0",
        ),
        (
            "reflect set receiver",
            "const proto = { set v(x) { this._v = x; } }; const target = Object.create(proto); const receiver = {}; Reflect.set(target, 'v', 5, receiver); console.log(receiver._v, target._v)",
            "5 undefined",
        ),
    ]);
}

#[test]
fn binary_data_and_text_encoding_work() {
    check(&[
        (
            "typed array kinds",
            "const i8 = new Int8Array([127, 128, -129]); const u8c = new Uint8ClampedArray([300, -5, 1.5]); const f32 = new Float32Array([0.1]); console.log(i8.join(), u8c.join(), f32[0] === Math.fround(0.1), Int32Array.BYTES_PER_ELEMENT)",
            "127,-128,127 255,0,2 true 4",
        ),
        (
            "shared buffers",
            "const buffer = new ArrayBuffer(8); const bytes = new Uint8Array(buffer); const words = new Uint32Array(buffer); words[0] = 0x01020304; console.log(bytes[0], bytes[3], buffer.byteLength, new Uint16Array(buffer, 2, 1)[0])",
            "4 1 8 258",
        ),
        (
            "subarray and slice",
            "const a = new Uint8Array([1, 2, 3, 4, 5]); const view = a.subarray(1, 3); view[0] = 9; const copy = a.slice(3); copy[0] = 0; console.log(a.join(), view.length, copy.join(), view.byteOffset)",
            "1,9,3,4,5 2 0,5 1",
        ),
        (
            "array methods",
            "const f = new Float64Array([3, 1, 2]); console.log(f.map((x) => x * 2).join(), f.filter((x) => x > 1).length, f.reduce((s, x) => s + x), f.indexOf(2), [...f.sort()].join(), f instanceof Float64Array, Object.prototype.toString.call(f))",
            "6,2,4 2 6 2 1,2,3 true [object Float64Array]",
        ),
        (
            "from and set",
            "const a = Int16Array.from([1, 2, 3], (x) => x * -1); const b = new Int16Array(5); b.set(a, 2); console.log(a.join(), b.join(), Uint8Array.of(1, 2).length)",
            "-1,-2,-3 0,0,-1,-2,-3 2",
        ),
        (
            "data view",
            "const view = new DataView(new ArrayBuffer(8)); view.setInt16(0, -2); view.setFloat32(4, 1.5, true); console.log(view.getInt16(0), view.getUint8(0), view.getUint8(1), view.getFloat32(4, true), ArrayBuffer.isView(view))",
            "-2 255 254 1.5 true",
        ),
        (
            "text encoding",
            "const bytes = new TextEncoder().encode('hé€'); console.log(bytes.length, bytes.join(), new TextDecoder().decode(bytes), new TextDecoder('latin1').decode(new Uint8Array([0x80, 0x41])))",
            "6 104,195,169,226,130,172 hé€ €A",
        ),
        (
            "streaming decode",
            "const d = new TextDecoder(); const part = new TextEncoder().encode('€'); console.log(JSON.stringify(d.decode(part.subarray(0, 2), { stream: true }) + '|' + d.decode(part.subarray(2))))",
            "\"|€\"",
        ),
        (
            "fatal decode",
            "try { new TextDecoder('utf-8', { fatal: true }).decode(new Uint8Array([0xff])); } catch (e) { console.log(e.name); }",
            "TypeError",
        ),
        (
            "encodeInto",
            "const target = new Uint8Array(4); const result = new TextEncoder().encodeInto('aé€', target); console.log(result.read, result.written, target.join())",
            "2 3 97,195,169,0",
        ),
        (
            "binary body",
            "new Response(new Uint16Array([0x6968])).text().then(console.log)",
            "hi",
        ),
    ]);
}

#[test]
fn intl_formats_numbers_dates_and_plurals() {
    check(&[
        (
            "number format",
            "console.log(new Intl.NumberFormat('en-US').format(1234567.891), new Intl.NumberFormat('de-DE').format(1234.5), (1234.5).toLocaleString('en-US', { minimumFractionDigits: 2 }))",
            "1,234,567.891 1.234,5 1,234.50",
        ),
        (
            "currency and percent",
            "console.log(new Intl.NumberFormat('en-US', { style: 'currency', currency: 'USD' }).format(-3.5), new Intl.NumberFormat('de-DE', { style: 'currency', currency: 'EUR' }).format(10), new Intl.NumberFormat('en', { style: 'percent' }).format(0.256), new Intl.NumberFormat('en', { style: 'currency', currency: 'JPY' }).format(1234.5))",
            "-$3.50 10,00\u{a0}€ 26% ¥1,235",
        ),
        (
            "bound format",
            "const f = new Intl.NumberFormat('en', { maximumFractionDigits: 1 }).format; console.log([1.25, 2.04].map(f).join('|'))",
            "1.3|2",
        ),
        (
            "date format",
            "const d = new Date(Date.UTC(2024, 0, 15, 15, 4, 5)); console.log(new Intl.DateTimeFormat('en-US', { timeZone: 'UTC' }).format(d), new Intl.DateTimeFormat('en-US', { dateStyle: 'long', timeZone: 'UTC' }).format(d), new Intl.DateTimeFormat('de-DE', { day: 'numeric', month: 'long', year: 'numeric', timeZone: 'UTC' }).format(d))",
            "1/15/2024 January 15, 2024 15. Januar 2024",
        ),
        (
            "date components",
            "const d = new Date(Date.UTC(2024, 6, 4, 9, 30)); console.log(d.toLocaleDateString('en-US', { weekday: 'long', month: 'short', day: 'numeric' }), d.toLocaleTimeString('en-US', { hour: 'numeric', minute: '2-digit' }), d.toLocaleString('en-GB', { timeZone: 'UTC' }))",
            "Thursday, Jul 4 9:30 AM 04/07/2024, 09:30:00",
        ),
        (
            "plural rules",
            "const p = new Intl.PluralRules('en-US'); const o = new Intl.PluralRules('en-US', { type: 'ordinal' }); console.log(p.select(1), p.select(2), o.select(22), o.select(13))",
            "one other two other",
        ),
        (
            "relative time",
            "const r = new Intl.RelativeTimeFormat('en', { numeric: 'auto' }); console.log(r.format(-1, 'day'), r.format(2, 'weeks'), new Intl.RelativeTimeFormat('en').format(-3, 'month'))",
            "yesterday in 2 weeks 3 months ago",
        ),
        (
            "collator",
            "const names = ['item10', 'Item2', 'item1']; names.sort(new Intl.Collator('en', { numeric: true, sensitivity: 'base' }).compare); console.log(names.join())",
            "item1,Item2,item10",
        ),
    ]);
}

#[test]
fn bigints_follow_the_specification() {
    check(&[
        (
            "arithmetic",
            "console.log(String(2n ** 64n), (10n / 3n) + '', String(-7n % 3n), String(123456789012345678901234567890n * 2n), typeof 1n)",
            "18446744073709551616 3 -1 246913578024691357802469135780 bigint",
        ),
        (
            "literals",
            "console.log(String(0xffn), String(0b101n), String(0o17n), String(1_000n))",
            "255 5 15 1000",
        ),
        (
            "comparison",
            "console.log(1n == 1, 1n === 1, 2n > 1, 1n < 1.5, 10n > '9', 0n == '', [3n, 1n, 2n].sort().join())",
            "true false true true true true 1,2,3",
        ),
        (
            "mixing throws",
            "try { 1n + 1; } catch (e) { console.log(e.name); } try { Math.max(1n); } catch (e) { console.log(e.name); } try { +1n; } catch (e) { console.log(e.name); }",
            "TypeError\nTypeError\nTypeError",
        ),
        (
            "conversions",
            "console.log(String(BigInt(42)), String(BigInt('0x10')), Number(2n ** 53n + 1n), `${10n}`, 5n + 'px', BigInt(true) + 0n)",
            "42 16 9007199254740992 10 5px 1n",
        ),
        (
            "conversion errors",
            "try { BigInt(1.5); } catch (e) { console.log(e.name); } try { BigInt('x'); } catch (e) { console.log(e.name); } try { new BigInt(1); } catch (e) { console.log(e.name); }",
            "RangeError\nSyntaxError\nTypeError",
        ),
        (
            "bitwise",
            "console.log(String(5n & 3n), String(-1n | 4n), String(~5n), String(1n << 70n), String(-9n >> 1n), String(BigInt.asIntN(8, 255n)), String(BigInt.asUintN(64, -1n)))",
            "1 -1 -6 1180591620717411303424 -5 -1 18446744073709551615",
        ),
        (
            "increment and negate",
            "let x = 9007199254740993n; x++; x--; --x; console.log(String(x), String(-x))",
            "9007199254740992 -9007199254740992",
        ),
        (
            "methods",
            "console.log((255n).toString(16), (-10n).toString(2), Object.prototype.toString.call(1n), (5n).valueOf() === 5n)",
            "ff -1010 [object BigInt] true",
        ),
        (
            "json",
            "try { JSON.stringify({ n: 1n }); } catch (e) { console.log(e.name); } BigInt.prototype.toJSON = function () { return this.toString(); }; console.log(JSON.stringify({ n: 7n }))",
            "TypeError\n{\"n\":\"7\"}",
        ),
        (
            "collections and typed arrays",
            "const m = new Map([[1n, 'a']]); const big = new BigInt64Array([-1n, 2n ** 63n]); const ubig = new BigUint64Array(1); ubig[0] = -1n; console.log(m.get(1n), String(big[0]), String(big[1]), String(ubig[0]), new DataView(big.buffer).getBigInt64(0, true) === -1n)",
            "a -1 -9223372036854775808 18446744073709551615 true",
        ),
        ("console", "console.log(10n, [1n])", "10n [ 1n ]"),
    ]);
}

#[test]
fn global_eval_runs_in_the_global_scope() {
    check(&[
        (
            "completion value",
            "console.log(eval('1 + 1'), eval('if (true) { 3 } else { 4 }'), eval('var q = 5; q * 2'), eval('1; if (false) 2'), eval('1; var z;'), eval('do { 3; break; } while (0)'), eval('try { 4 } finally { 5 }'), eval('let i = 0; while (i < 3) i++'))",
            "2 3 10 undefined 1 3 4 2",
        ),
        (
            "declarations become globals",
            "(function () { var local = 1; (0, eval)('var leaked = typeof local'); })(); console.log(leaked, globalThis.leaked)",
            "undefined undefined",
        ),
        (
            "non-strings pass through",
            "const o = {}; console.log(eval(o) === o, eval(7), eval(), eval.length, eval.name)",
            "true 7 undefined 1 eval",
        ),
        (
            "syntax errors",
            "try { eval('let = ;'); } catch (e) { console.log(e instanceof SyntaxError) }",
            "true",
        ),
    ]);
}

#[test]
fn anonymous_classes_take_the_name_they_are_bound_to() {
    check(&[(
        "class names",
        "var A = class {}; var B = class { constructor() {} }; let C = class { m() {} }; const o = { D: class { constructor() {} } }; class E { constructor() {} } console.log(A.name, B.name, C.name, o.D.name, E.name, (class { constructor() {} }).name === '')",
        "A B C D E true",
    )]);
}
