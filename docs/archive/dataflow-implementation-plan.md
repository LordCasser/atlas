# Atlas 多语言 Dataflow 完整化技术路线与验收方案

> 本文以 Atlas 当前架构与需求文档为准，目标不是将能力声明降级，而是说明：如果希望下一轮修改后让“基本所有支持语言”都达到 dataflow 级别，每个语言需要补什么、怎么补、如何验收，以及当前非文档类问题的原因与解决方案。

## 0. Atlas 语境下的“完整 dataflow”

Atlas 的 dataflow 不应被理解为完整编译器级 SSA、类型系统、别名分析或精确指针分析。按照当前需求，Atlas 的核心目标更接近：

```text
用户指定变量 / 表达式 / 调用实参
  -> 找到本地定义
  -> 找到赋值来源
  -> 找到字段访问来源
  -> 找到调用参数
  -> 找到返回值来源
  -> 能解释路径、置信度、provenance、diagnostics
```

建议将“完整 dataflow”拆成三个可验收层级。

### L3：语言内本地 dataflow 完整

每个函数内至少支持：

- 参数节点：`Parameter`
- 局部变量节点：`Local`
- 字段 / 成员访问节点：`Field`
- 字面量节点：`Literal`
- 一般表达式节点：`Expr`
- 调用目标节点：`CallTarget`
- 调用实参节点：`CallArg`
- 返回值节点：`Return`
- 赋值边：`Assign`
- 读取边：`Read`
- 字段读取边：`FieldLoad`
- 字段写入边：`FieldStore`
- 调用实参到调用节点：建议改名为 `ArgToCall`
- 返回表达式到返回节点：建议新增 `ReturnValue` 或修正当前 `ReturnToCall` 语义

核心验收示例：

```text
function f(a, b) {
  x = a
  y = x.field
  z = call(y, b)
  return z
}
```

trace `z` 或 return 时应能得到：

```text
return z -> z -> call(y, b) -> y -> x.field -> x -> a
```

### L4：控制流敏感的本地 dataflow

支持：

- if / else 分支 merge
- loop 中变量更新
- early return
- try/catch/finally
- switch/match/when
- 基础 CFG
- Phi / merge 节点，或以多路径 provenance 表示

L4 不一定要一次做完，但如果对外宣称 `DataflowFull`，至少需要有条件分支和循环的 fixture。

### L5：轻量跨函数 dataflow

支持：

- caller argument -> callee parameter
- callee return -> caller call result
- receiver -> this/self
- basic function summary
- bounded depth
- recursion cut-off
- confidence / partial_result

示例：

```ts
function source(req) { return req.body.name }
function wrap(x) { return sanitize(x) }
function sink(v) {}

sink(wrap(source(req)))
```

应能 trace：

```text
sink arg
 -> wrap return
 -> wrap param x
 -> source return
 -> req.body.name
```

---

## 1. 公共架构改造路线

当前很多语言已经有 `dataflow_builder.scm`，但真正建边的 `DataFlowBuilder` 只懂少数 AST：

```rust
variable_declarator
assignment_expression
assignment
```

所以仅增加 query 不够。要达成多语言 dataflow，需要先完成公共架构改造。

### 1.1 `DataflowSpec` 增加语言专属 edge builder

当前接口：

```rust
trait DataflowSpec {
    fn dataflow_builder_query(&self) -> &str;
    fn normalize(...) -> (Option<DataNode>, Option<DataFlowEdge>);
}
```

问题：

- query 只负责捕获节点；
- 真正的 `Assign` / `FieldLoad` / `ArgToParam` / `ReturnToCall` 边在通用 builder 中创建；
- 通用 builder 不了解 Go / Rust / C# / Kotlin / PHP / Ruby 等语言 AST。

建议改为：

```rust
pub trait DataflowSpec: Send + Sync {
    fn dataflow_builder_query(&self) -> &str;

    fn normalize(
        &self,
        ctx: NormalizeCtx<'_>,
        capture: Capture<'_>,
    ) -> (Option<DataNode>, Option<DataFlowEdge>);

    fn build_language_edges(
        &self,
        ctx: &ExtractionCtx<'_>,
        nodes: &[DataNode],
        bindings: &[BindingDef],
        scopes: &[ScopeDef],
        pos_map: &NodePosMap,
        edges: &mut Vec<DataFlowEdge>,
    ) -> anyhow::Result<()> {
        build_default_edges(...)
    }
}
```

然后分语言实现：

- TS/JS：`TsDataflowEdges`
- Python：`PythonDataflowEdges`
- Java：`JavaDataflowEdges`
- Go：`GoDataflowEdges`
- Rust：`RustDataflowEdges`
- 其他语言类似

### 1.2 统一 `DataNode` normalize 语义

当前存在明显语义不一致，例如：

```rust
"df.receiver" => DataNodeKind::Literal
```

应统一为：

| capture | DataNodeKind |
|---|---|
| `df.parameter` | `Parameter` |
| `df.assign_target` | `Local` / `Field` / `Global` |
| `df.assign_value` | `Expr` |
| `df.identifier_use` | `VariableUse` 或 `Expr` |
| `df.receiver` | `Receiver` |
| `df.call_target` | `CallTarget` |
| `df.call_arg` | `CallArg` |
| `df.return_value` | `Return` 或 `Expr + Return` |
| `df.field_name` | `Field` |
| `df.literal` | `Literal` |

建议新增 `DataNodeKind::VariableUse`。如果暂不新增枚举，也至少要约定：

```text
identifier use -> Expr(name = identifier)
compound expr -> Expr(name = full expression)
```

### 1.3 补表达式内部 identifier 分解

当前最大精度问题是：

```ts
let result = a + b;
```

通常只有：

```text
Expr("a + b") -> Local("result")
```

但没有：

```text
Parameter("a") -> Expr("a + b")
Parameter("b") -> Expr("a + b")
```

公共方案：

1. 每个语言 query 捕获 identifier use：

   ```scheme
   (identifier) @df.identifier_use
   ```

2. normalize 阶段排除：
   - declaration name
   - property name
   - type name
   - import name
   - 已单独捕获的 callee target

3. 生成 `VariableUse` 或 `Expr`。

4. 对包含关系建边：

   ```text
   VariableUse(a) -> Expr(a + b)
   VariableUse(b) -> Expr(a + b)
   Expr(a + b) -> Local(result)
   ```

5. 通过 `binding_id` 把 variable use 连回 definition：

   ```text
   Parameter(a) -> VariableUse(a)
   Parameter(b) -> VariableUse(b)
   ```

每种语言都需要验收：

```text
result = a + b * c
return result
```

trace `result` 必须看到 `a`、`b`、`c`。

### 1.4 lexical binding 必须真正 scope-aware

当前 `LexicalBinder` 能给 binding 找 innermost scope，但仍有问题：

- 部分语言 lexical query 过粗；
- 重新赋值被当成新 binding；
- identifier use 后扫需要更精准排除 declaration；
- `function_id` 没有在 binding 层稳定填充；
- scope query 对 C/C++/Kotlin 等不完整，导致 `function_id` 为空。

建议：

#### `BindingDef` 只用于定义点

例如 PHP 当前：

```scheme
(assignment_expression
  left: (variable_name) @lexical.local)
```

会把每次 `$x = ...` 都当成新 binding。应区分：

- 首次局部定义
- 后续赋值
- 全局变量
- 参数
- catch / foreach / pattern binding

动态语言可先用启发式：

```text
同一 scope 中第一次 assignment LHS 视作 BindingDef；
之后同名 LHS 视作 AssignmentTarget，不再新增 BindingDef。
```

#### `BindingUse` 用通用 identifier-use scanner 生成

针对每种语言排除类型名、字段名、声明名。

验收：

```text
let x = source()
{
  let x = other()
  sink(x)
}
sink(x)
```

内部 sink 必须来自 `other()`，外部 sink 必须来自 `source()`。

### 1.5 统一 `callsite_id`

每个语言需要稳定实现：

```rust
fn find_enclosing_call_expression(lang, node) -> Option<Node>
```

语言对应 call expression kind：

| 语言 | call kind |
|---|---|
| TS/JS | `call_expression`, `new_expression` |
| Python | `call` |
| Java | `method_invocation`, `object_creation_expression`, `constructor_invocation` |
| C/C++ | `call_expression` |
| Go | `call_expression` |
| C# | `invocation_expression`, `object_creation_expression` |
| Rust | `call_expression`, `macro_invocation`, method call via `call_expression` + `field_expression` |
| PHP | `function_call_expression`, `member_call_expression`, `object_creation_expression` |
| Ruby | `call`, `command`, `method_call` |
| Kotlin | `call_expression`, constructor call, navigation call |

验收：

```text
foo(bar(a), baz(b, c), d)
```

必须保证：

- `a` 属于 `bar`
- `b`, `c` 属于 `baz`
- `bar(a)`, `baz(b,c)`, `d` 属于 `foo`

不能再依赖 “most recent preceding target”。

### 1.6 修正 `ArgToParam` / `ReturnToCall` 语义

当前 intra-procedural builder 实际把：

```text
call_arg -> call_target
```

命名成 `ArgToParam`；又把：

```text
expr inside return -> return node
```

命名成 `ReturnToCall`。

建议区分：

| 当前 | 问题 | 建议 |
|---|---|---|
| `ArgToParam` | 实际是 `CallArg -> CallTarget`，不是 formal parameter | 新增 `ArgToCall`，真正跨函数再用 `ArgToParam` |
| `ReturnToCall` | 当前也用于 `expr -> Return`，不是 callee return 到 caller call result | 新增 `ReturnValue` / `ReturnExpr`，跨函数再用 `ReturnToCall` |

建议模型：

```text
local intra-procedural:
  expr -> local                  Assign
  variable_use -> expr           Read
  base -> field                  FieldLoad
  value -> field                 FieldStore
  arg_expr -> call_arg           Read / Assign
  call_arg -> call_target        ArgToCall
  return_expr -> return_node     ReturnValue

inter-procedural virtual:
  caller call_arg -> callee param      ArgToParam
  callee return -> caller call_result  ReturnToCall
  caller receiver -> callee this       ReceiverToThis
```

### 1.7 统一函数范围

`resolve_dataflow_function_ids()` 依赖 symbol range 包含 DataNode。

部分语言 definitions query 只捕获函数名，scope query 有些只捕获 body，例如 C/C++：

```scheme
(function_definition (compound_statement) @scope.function)
```

body 不包含函数名，导致函数 symbol range 无法可靠扩成完整函数范围。

建议每个语言都产出：

```text
FunctionLikeRange {
  symbol_name_range,
  full_function_range,
  body_range,
}
```

如果暂不改 schema，至少保证：

- function/method/constructor 的 `SymbolDef.range` 是 full function range；
- name range 后续另存。

验收：

```text
每个语言中，函数体内所有 DataNode.function_id 必须等于所在函数 SymbolId。
```

---

## 2. 各语言技术路线与验收

## 2.1 TypeScript

### 当前缺口

- destructuring 不支持；
- property / element assignment 不支持；
- receiver 被当成 Literal；
- 表达式内部 identifier 未完整分解；
- optional chaining / nullish coalescing / await / new / constructor flow 不完整；
- `ArgToParam` 语义错误；
- class field / `this.field` dataflow 不完整。

### 技术路线

#### 阶段 1：修正基础节点语义

修改 `normalize_ts_dataflow_builder`：

- `df.receiver` -> `DataNodeKind::Receiver`
- `df.await_value` 不应作为 Literal，建议生成 `Expr(await expr)` 并建立 awaited expr -> await expr 的 `Read` 边
- 区分 method call property 与普通 field：
  - `obj.method()` 的 `method` 是 `CallTarget`
  - `obj.field` 的 `field` 是 `Field`

#### 阶段 2：补 query

需要支持：

```ts
const { a, b: c } = obj;
const [x, y] = arr;
obj.field = value;
this.field = value;
arr[i] = value;
const x = await source();
const x = new Foo(arg);
return await x;
```

新增 captures：

```scheme
;; destructuring binding
(object_pattern (shorthand_property_identifier_pattern) @df.assign_target)
(pair_pattern key: (_) value: (identifier) @df.assign_target)
(array_pattern (identifier) @df.assign_target)

;; property assignment
(assignment_expression
  left: (member_expression) @df.assign_field_target
  right: (_) @df.assign_value)

;; element assignment
(assignment_expression
  left: (subscript_expression) @df.assign_field_target
  right: (_) @df.assign_value)

;; new expression
(new_expression
  constructor: (_) @df.call_target
  arguments: (arguments (_) @df.call_arg))

;; identifier uses
(identifier) @df.identifier_use
```

#### 阶段 3：TS 专属 edge builder

实现：

- `variable_declarator`: value -> target
- `assignment_expression`: right -> left
- `member_expression` read: object -> field
- `member_expression` write: right -> field
- `subscript_expression`: object/index -> element access
- destructuring:
  - object -> property field
  - property -> local
- `await`: awaited expr -> await expr
- `return`: expr -> return
- `call`: args -> call target / call result

### 验收

新增 fixtures：

1. `ts_simple_assignment.ts`
2. `ts_expression_identifiers.ts`
3. `ts_destructuring.ts`
4. `ts_field_read_write.ts`
5. `ts_nested_call_args.ts`
6. `ts_async_await.ts`
7. `ts_constructor_new.ts`
8. `ts_shadowing.ts`
9. `ts_optional_chain.ts`
10. `ts_cross_function_summary.ts`

示例：

```ts
function f(req: any) {
  const { name } = req.body;
  const clean = sanitize(name);
  return clean;
}
```

trace `clean` 必须看到：

```text
clean <- sanitize(name) <- name <- req.body.name <- req
```

---

## 2.2 JavaScript

### 当前缺口

- 与 TypeScript 共享大多数缺口；
- 单独 `javascript` feature 编译失败；
- CommonJS `require` / `module.exports` dataflow 未建模；
- dynamic property access 更常见；
- prototype / this 语义未处理；
- default parameter / rest parameter / object spread 未完整处理。

### 技术路线

#### 阶段 1：修 feature gating

当前 `javascript.rs` 依赖：

```rust
super::typescript::normalize_ts_...
```

但 `javascript` feature 没有强制包含 `typescript` module。

短期：

```toml
javascript = ["typescript", "dep:tree-sitter-typescript"]
```

长期：

```text
languages/ecmascript_shared.rs
```

TS/JS/ArkTS 都依赖 shared normalize helpers。

#### 阶段 2：补 JS 特有 query

支持：

```js
const x = require("pkg")
module.exports = x
exports.foo = foo
const { a } = obj
function f(...args) {}
const x = obj?.field
const y = obj[key]
```

#### 阶段 3：JS 专属 edge 规则

- `require("x") -> local alias`
- `module.exports.foo = value`: value -> export field
- rest parameter -> parameter node with access_path `args.*`
- `fn(...args)`: args -> call_arg
- optional chain：object -> field，confidence 降低

### 验收

JS fixtures：

1. CommonJS require
2. ESM import
3. destructuring
4. dynamic property
5. nested calls
6. rest/spread
7. module.exports

示例：

```js
const input = req.body.name;
module.exports.handler = () => sink(input);
```

trace `sink(input)` 必须能到 `req.body.name`。

---

## 2.3 Python

### 当前缺口

- 没有 lexical binding；
- `callsite_id` 没设置；
- tuple/list unpacking 不支持；
- attribute assignment 不支持；
- subscript dataflow 不支持；
- keyword args 不支持；
- `*args` / `**kwargs` 不完整；
- comprehension / with / for target 不支持；
- import alias 到变量来源不足。

### 技术路线

#### 阶段 1：实现 Python lexical binding

新增 `queries/python/lexical.scm`：

```scheme
;; parameters
(parameters (identifier) @lexical.parameter)
(default_parameter (identifier) @lexical.parameter)
(typed_parameter (identifier) @lexical.parameter)
(typed_default_parameter (identifier) @lexical.parameter)
(list_splat_pattern (identifier) @lexical.parameter)
(dictionary_splat_pattern (identifier) @lexical.parameter)

;; assignment targets
(assignment left: (identifier) @lexical.local)

;; loop targets
(for_statement left: (identifier) @lexical.local)

;; with alias
(with_item alias: (identifier) @lexical.local)

;; except alias
(except_clause alias: (identifier) @lexical.catch_variable)

;; import alias
(aliased_import alias: (identifier) @lexical.import_alias)
```

tuple unpacking：

```python
a, b = pair
```

需要捕获 pattern 中每个 identifier。

#### 阶段 2：设置 `callsite_id`

在 `normalize_py_dataflow_builder` 中，对：

- `df.call_arg`
- `df.call_target`
- call 内部 `df.assign_value`

都通过：

```rust
find_call_expression_python(node)
```

寻找 `call` ancestor。

#### 阶段 3：补 query

支持：

```python
self.x = value
obj.attr = value
arr[i] = value
a, b = pair
for x in items:
with open(p) as f:
except Exception as e:
foo(x=bar)
foo(*args, **kwargs)
return await x
```

#### 阶段 4：Python edge builder

处理：

- `assignment`: right -> left
- tuple/list pattern:
  - `pair -> a`
  - `pair -> b`
  - 或更细：`pair[0] -> a`
- attribute read:
  - `obj -> obj.attr`
- attribute write:
  - `value -> obj.attr`
- subscript read/write:
  - `obj -> obj[index]`
  - `index -> obj[index]`
  - `value -> obj[index]`
- keyword args：
  - call_arg 保存 `arg.name`
- `for x in iterable`:
  - iterable -> x，kind 可用 `Assign`，confidence 0.65
- `with expr as x`:
  - expr -> x

### 验收

Python fixtures：

1. simple assignment
2. expression identifiers
3. tuple unpacking
4. attribute read/write
5. subscript read/write
6. nested call
7. keyword args
8. for loop target
9. with alias
10. import alias
11. shadowing
12. cross-function summary

示例：

```python
def f(req):
    name = req.body["name"]
    clean = sanitize(name)
    return clean
```

trace `clean` 必须到：

```text
clean <- sanitize(name) <- name <- req.body["name"] <- req
```

---

## 2.4 Java

### 当前缺口

- constructor / object creation 不完整；
- field assignment 不完整；
- array access 不完整；
- enhanced for、lambda、try-with-resources 不完整；
- method invocation receiver / static call 区分不足；
- `this.field` / `Class.field` access_path 不完整；
- generics / overload 不要求精确，但需要低置信度 diagnostics。

### 技术路线

#### 阶段 1：Java edge builder

处理 AST：

- `local_variable_declaration`
- `variable_declarator`
- `assignment_expression`
- `field_access`
- `array_access`
- `method_invocation`
- `object_creation_expression`
- `return_statement`
- `lambda_expression`
- `enhanced_for_statement`
- `try_with_resources_statement`

#### 阶段 2：补 query

新增捕获：

```scheme
(object_creation_expression
  type: (_) @df.call_target
  arguments: (argument_list (_) @df.call_arg))

(assignment_expression
  left: (field_access) @df.assign_field_target
  right: (_) @df.assign_value)

(assignment_expression
  left: (array_access) @df.assign_field_target
  right: (_) @df.assign_value)

(array_access
  array: (_) @df.receiver
  index: (_) @df.index)

(enhanced_for_statement
  name: (identifier) @df.assign_target
  value: (_) @df.assign_value)

(lambda_expression
  parameters: (_) @df.parameter)
```

#### 阶段 3：receiver / this 处理

Java method invocation：

```java
obj.method(arg)
method(arg)
Class.staticMethod(arg)
```

要区分：

- receiver = `obj`
- call_target = `obj.method`
- static access_path = `Class.staticMethod`
- implicit this call = `this.method`

### 验收

Java fixtures：

1. local assignment
2. field read/write
3. array read/write
4. constructor call
5. nested method calls
6. lambda
7. enhanced for
8. try-with-resources
9. shadowing
10. cross-method summary

示例：

```java
class A {
  String f(Request req) {
    String name = req.body.name;
    String clean = sanitize(name);
    return clean;
  }
}
```

trace `clean` 必须到 `req.body.name` 和 `req` parameter。

---

## 2.5 C

### 当前缺口

- `languages/c.rs` 编译失败，缺少 brace；
- `function_id` 可能无法正确设置，因为 function scope 捕获 body；
- `init_declarator` 不被 generic builder 处理；
- pointer / address / dereference / struct field / array / function pointer 未建模；
- macros / preprocessing 不做完整支持，但要有 diagnostics。

### 技术路线

#### 阶段 0：修编译

`impl DataflowSpec for CAdapter` 补 `}`。

增加 CI：

```bash
cargo check -p extraction --no-default-features --features c
cargo check -p atlas-cli --features all-languages
```

#### 阶段 1：修 function range

scope query 改为捕获完整函数：

```scheme
(function_definition) @scope.function
```

不要只捕获 compound_statement。

#### 阶段 2：C edge builder

处理：

- `init_declarator`: value -> declarator
- `assignment_expression`: right -> left
- pointer declarator：`int *p = &x`
- unary expressions：`&x`、`*p`
- field access：`obj.field`、`ptr->field`
- array subscript：`arr[i]`
- call expression：function pointer call、normal call
- return statement

#### 阶段 3：指针 conservative model

不建议一开始做完整 alias analysis，但至少做：

```text
&x -> p          AddressOf
p -> *p          DerefRead
value -> *p      DerefWrite
```

若不新增 edge kind，可用：

- `Read`
- `Write`
- `Assign`
- `FieldLoad`
- `FieldStore`

但 provenance 要写清楚：

```text
provenance: c_pointer_heuristic
confidence: 0.45
```

### 验收

C fixtures：

1. init declarator
2. assignment
3. struct field dot
4. pointer field arrow
5. array access
6. pointer deref read/write
7. function call args
8. return value
9. function pointer low confidence
10. macro unsupported diagnostic

示例：

```c
int f(Request *req) {
  char *name = req->body.name;
  return sink(name);
}
```

trace `name` 必须到 `req->body.name` 和 parameter `req`。

---

## 2.6 C++

### 当前缺口

C++ 继承 C 的大部分问题，另外还有：

- references / move / copy 不建模；
- constructors / initializer list 不支持；
- method receiver 不完整；
- templates / overload / ADL 不精确；
- lambda capture 不支持；
- smart pointer `ptr->field` / `(*ptr).field` 未统一。

### 技术路线

#### 阶段 1：function range

同 C，scope 捕获完整 `function_definition`。

#### 阶段 2：C++ edge builder

处理：

- `init_declarator`
- `assignment_expression`
- `field_expression`
- `subscript_expression`
- `call_expression`
- `new_expression`
- constructor initializer list
- reference declaration：

```cpp
int &r = x;
```

建：

```text
x -> r
```

- lambda captures：

```cpp
[x, &y](int z) { ... }
```

建 capture source 到 lambda param/field 的低置信边。

#### 阶段 3：类型复杂度降级但不阻断

模板、重载、ADL 不要求精准，但要：

- 保留候选 call target
- confidence 降低
- diagnostic 标注

### 验收

C++ fixtures：

1. local init
2. reference binding
3. pointer field
4. object field
5. method call receiver
6. constructor call
7. initializer list
8. lambda capture
9. template call best-effort
10. overload diagnostic

示例：

```cpp
std::string f(Request& req) {
  auto name = req.body.name;
  return sanitize(name);
}
```

trace `name` 到 `req.body.name`。

---

## 2.7 ArkTS

### 当前缺口

ArkTS 复用 TypeScript grammar，但 ArkTS 特有语义不处理：

- ArkUI decorators
- struct/component lifecycle
- state/prop/link/storage annotations
- `.ets` 特有 UI DSL
- ability/page 生命周期

### 技术路线

#### 阶段 1：抽 ECMAScript shared

TS/JS/ArkTS 共用：

```text
ecmascript_shared.rs
```

但 ArkTS adapter 单独加 provenance：

```text
arkts_via_typescript_grammar
```

#### 阶段 2：ArkTS 特有 query

至少支持：

```arkts
@State message: string = ''
@Prop value: string
@Builder
build() { ... }
Button(this.message)
```

要将：

- `@State field`
- `@Prop field`
- class/struct field
- lifecycle method
- builder function

作为 dataflow 节点。

#### 阶段 3：UI DSL call args

ArkTS UI DSL 中很多像函数调用：

```arkts
Text(this.message)
Button('OK').onClick(() => ...)
```

需要把：

- `Text(...)`
- chained method call
- lambda callback args

纳入 callsite。

### 验收

ArkTS fixtures：

1. TS subset
2. `@State` field read/write
3. `@Prop`
4. UI component call args
5. chained method
6. callback lambda
7. lifecycle method

示例：

```arkts
@State name: string = getName()
build() {
  Text(this.name)
}
```

trace `this.name` 到 `getName()`。

---

## 2.8 Go

### 当前缺口

Go query 捕获不少，但 edge builder 不支持 Go 关键 AST：

- `short_var_declaration`
- `assignment_statement`
- `var_spec`
- `expression_list`
- multi-return
- range loop
- selector expression
- pointer deref
- goroutine / defer

### 技术路线

#### 阶段 1：Go edge builder

处理：

```go
x := expr
x = expr
var x = expr
a, b := f()
a, err = g()
return x, y
for k, v := range m
obj.field
ptr.field
arr[i]
go sink(x)
defer sink(x)
```

#### 阶段 2：多值赋值建模

Go 最重要的是 multi-value：

```go
a, b := f()
```

需要生成：

```text
CallReturn(f)[0] -> a
CallReturn(f)[1] -> b
```

如果暂时没有 `CallReturn(index)`，可先：

```text
f() Expr -> a
f() Expr -> b
```

但 confidence 降低。

建议扩展 `DataNode`：

```rust
return_index: Option<u32>
```

或者复用 `arg_index`，但语义不佳。

#### 阶段 3：range flow

```go
for _, v := range items
```

建：

```text
items -> v
```

confidence 0.65。

#### 阶段 4：defer/go

```go
defer sink(x)
go sink(x)
```

仍然是 callsite，但 provenance 标注：

```text
call_kind: deferred / goroutine
```

### 验收

Go fixtures：

1. short var
2. assignment
3. var spec
4. multi-return
5. selector field
6. array/slice index
7. range loop
8. method receiver
9. defer
10. go routine
11. shadowing

示例：

```go
func f(req Request) string {
    name := req.Body.Name
    clean := sanitize(name)
    return clean
}
```

trace `clean` 到 `req.Body.Name`。

---

## 2.9 C#

### 当前缺口

- local declaration initializer 不一定建 Assign edge；
- property / indexer assignment 不完整；
- object creation 不完整；
- async/await 不完整；
- LINQ / lambda 不完整；
- nullable / pattern matching 不支持；
- `this.Field` / static member 未稳定建 access_path。

### 技术路线

#### 阶段 1：C# edge builder

处理：

- `local_declaration_statement`
- `variable_declarator`
- `equals_value_clause`
- `assignment_expression`
- `member_access_expression`
- `element_access_expression`
- `invocation_expression`
- `object_creation_expression`
- `return_statement`
- `await_expression`
- `lambda_expression`
- `foreach_statement`
- `using_statement`

#### 阶段 2：补 query

支持：

```csharp
var x = expr;
obj.Prop = value;
arr[i] = value;
new Foo(arg);
await FooAsync();
foreach (var x in xs) {}
using var r = Open();
```

#### 阶段 3：LINQ conservative dataflow

```csharp
xs.Select(x => f(x)).Where(y => ...)
```

初期可以：

- lambda param 从 collection item 来；
- lambda return 到 chained call result；
- confidence 0.45-0.6。

### 验收

C# fixtures：

1. local declaration
2. property read/write
3. indexer
4. object creation
5. async await
6. lambda
7. LINQ basic
8. foreach
9. using var
10. shadowing

示例：

```csharp
string F(Request req) {
    var name = req.Body.Name;
    var clean = Sanitize(name);
    return clean;
}
```

trace `clean` 到 `req.Body.Name`。

---

## 2.10 Rust

### 当前缺口

- `let_declaration` 不被 generic builder 处理；
- expression tail return 不支持；
- pattern destructuring 不支持；
- match binding 不完整；
- method call / associated function call 区分；
- borrow/deref/move 语义未建模；
- closure capture 不支持；
- macro 不支持。

### 技术路线

#### 阶段 1：Rust edge builder

处理：

```rust
let x = expr;
x = expr;
return expr;
tail_expr
obj.field
obj.method(arg)
Type::assoc(arg)
&x
*x
for x in iter
match value { Some(x) => ... }
if let Some(x) = opt
closure |x| ...
```

#### 阶段 2：tail expression return

Rust 必须支持：

```rust
fn f(x: i32) -> i32 {
    x + 1
}
```

需要识别 function body 最后一个 expression 且无 semicolon，建：

```text
tail_expr -> Return
```

#### 阶段 3：pattern binding

支持：

```rust
let (a, b) = pair;
let Some(x) = opt else { ... };
match opt {
  Some(x) => x,
  None => ...
}
```

初期 conservative：

```text
pair -> a
pair -> b
opt -> x
```

#### 阶段 4：borrow/deref

建低置信边：

```text
x -> &x
&x -> ref
ref -> *ref
```

不要试图实现 borrow checker。

#### 阶段 5：macro diagnostics

宏调用：

```rust
foo!(x)
```

作为 callsite low confidence，不展开。

### 验收

Rust fixtures：

1. let init
2. assignment
3. tail return
4. explicit return
5. field read
6. method call
7. associated call
8. destructuring
9. match binding
10. if let
11. borrow/deref
12. closure capture
13. macro low-confidence diagnostic

示例：

```rust
fn f(req: Request) -> String {
    let name = req.body.name;
    sanitize(name)
}
```

trace tail return 到 `req.body.name`。

---

## 2.11 PHP

### 当前缺口

- assignment LHS 被当成 lexical binding，导致重复定义；
- array access 是核心来源但未覆盖；
- superglobals 未建模；
- object property / static property / method call 不完整；
- dynamic call / variable variable 不可精确；
- namespace alias 需要结合 import resolver。

### 技术路线

#### 阶段 1：修 lexical model

不要把每个 assignment 都直接当 BindingDef。需要：

- parameter -> BindingDef
- foreach value -> BindingDef
- catch variable -> BindingDef
- static variable -> BindingDef
- assignment LHS：
  - 如果当前 scope 第一次出现，建立 BindingDef
  - 后续只作为 DataNode Local target

#### 阶段 2：补 query

支持：

```php
$x = expr;
$obj->field;
$obj->field = value;
$arr[$key];
$arr[$key] = value;
$_GET["name"];
self::$field;
ClassName::method($x);
new Foo($x);
foo($x);
$obj->method($x);
```

#### 阶段 3：superglobal source modeling

对：

```php
$_GET
$_POST
$_REQUEST
$_SERVER
$_COOKIE
$_FILES
```

建立 `Global` DataNode，provenance 标注 external input。

#### 阶段 4：dynamic call diagnostics

```php
$fn($x);
$obj->$method($x);
```

生成 callsite，但：

```text
confidence: 0.35
diagnostic: dynamic_call_unresolved
```

### 验收

PHP fixtures：

1. parameter/local
2. array access
3. superglobal
4. object property read/write
5. static access
6. function call
7. method call
8. constructor
9. foreach
10. dynamic call diagnostic
11. namespace use alias

示例：

```php
function f($req) {
    $name = $_GET["name"];
    $clean = sanitize($name);
    return $clean;
}
```

trace `$clean` 到 `$_GET["name"]`。

---

## 2.12 Ruby

### 当前缺口

- implicit return 不支持；
- call 和 field access 语义混合；
- instance variable / class variable / global variable 不支持；
- block params / yield 不完整；
- method_missing / define_method 动态不可精确；
- hash access 常见但未建模。

### 技术路线

#### 阶段 1：Ruby lexical binding

支持：

- method params
- optional params
- keyword params
- block params
- local assignment
- rescue var
- for var

#### 阶段 2：implicit return

Ruby 必须支持：

```ruby
def f(x)
  x + 1
end
```

最后表达式 -> Return。

#### 阶段 3：区分 method call vs field-like call

Ruby 没有普通字段访问语法，`obj.foo` 是方法调用。不能简单当 FieldLoad。

策略：

- `@ivar` / `@@cvar` / `$global` 当 Field/Global；
- `obj.foo` 默认 `CallTarget`；
- 如果是 `attr_reader` / `attr_accessor`，可低置信标为 FieldLoad；
- 否则不要把 method call 伪装成字段读取。

#### 阶段 4：hash/index access

支持：

```ruby
params[:name]
params["name"]
obj[:key]
```

建 Field/Element access_path。

#### 阶段 5：blocks/yield

```ruby
items.map { |x| sanitize(x) }
yield value
```

初期：

- collection -> block param，confidence 0.55
- block return -> call result，confidence 0.45

### 验收

Ruby fixtures：

1. explicit return
2. implicit return
3. local assignment
4. instance variable
5. hash access
6. method call
7. block param
8. rescue variable
9. attr_reader best-effort
10. dynamic method diagnostic

示例：

```ruby
def f(params)
  name = params[:name]
  clean = sanitize(name)
  clean
end
```

trace implicit return 到 `params[:name]`。

---

## 2.13 Kotlin

### 当前缺口

- function scope 没捕获完整 function；
- property/variable declaration initializer 不被 generic builder 处理；
- expression body function 不支持；
- safe call / elvis / scope functions 不支持；
- destructuring / data class component 不支持；
- lambda / `it` 参数不完整。

### 技术路线

#### 阶段 1：修 scope

`queries/kotlin/scopes.scm` 必须捕获：

```scheme
(function_declaration) @scope.function
```

而不仅是 `function_body`。

#### 阶段 2：Kotlin edge builder

处理：

```kotlin
val x = expr
var x = expr
x = expr
return expr
fun f(x: T) = expr
obj.field
obj?.field
arr[i]
foo(arg)
obj.method(arg)
Foo(arg)
lambda { x -> ... }
items.map { it.name }
for (x in xs)
```

#### 阶段 3：expression body return

```kotlin
fun f(x: Int) = x + 1
```

必须建：

```text
expr -> Return
```

#### 阶段 4：safe call / elvis

```kotlin
val x = obj?.field ?: default
```

建两条来源：

```text
obj.field -> x
default -> x
```

confidence 标注 conditional。

#### 阶段 5：scope functions

支持常见：

```kotlin
value.let { sanitize(it) }
obj.apply { field = v }
obj.run { field }
```

初期 best-effort：

- receiver -> implicit `it` / `this`
- block return -> call result

### 验收

Kotlin fixtures：

1. val/var init
2. assignment
3. expression body
4. safe call
5. elvis
6. field read/write
7. method call
8. lambda explicit param
9. lambda implicit it
10. for loop
11. scope functions
12. destructuring

示例：

```kotlin
fun f(req: Request): String {
    val name = req.body.name
    val clean = sanitize(name)
    return clean
}
```

trace `clean` 到 `req.body.name`。

---

## 2.14 Bash

如果希望 Bash 达到 dataflow，需要接受它只能是 shell-script best-effort。

### 技术路线

支持：

```bash
x=$(cmd)
x="$1"
source file.sh
foo "$x"
export X="$x"
```

建模：

- assignment -> variable
- parameter `$1`, `$@`, `$*` -> external input
- command substitution -> call result
- source -> import/include
- env var -> global
- pipe：上一命令 stdout -> 下一命令 stdin

### 验收

```bash
name="$1"
clean=$(sanitize "$name")
sink "$clean"
```

trace `clean` 到 `$1`，但必须低置信度。

---

## 2.15 Cangjie

Cangjie 当前 grammar 是 optional，需要先确认 AST 稳定性。

路线类似 Kotlin/Swift：

- function range
- variable declaration
- assignment
- member access
- call args
- return
- lambda
- pattern destructuring

建议放在最后，因为它不属于默认 `all-languages`。

---

## 3. 跨函数 dataflow 技术路线

如果只做 L3，每个语言 trace 只能在函数内跑。要真正接近完整 dataflow，需要做 L5。

### 3.1 FunctionSummary

为每个函数构建 summary：

```rust
FunctionSummary {
    function_id,
    param_to_return: Vec<Path>,
    param_to_call_arg: Vec<Path>,
    field_to_return: Vec<Path>,
    receiver_to_return: Vec<Path>,
    global_to_return: Vec<Path>,
}
```

来源于函数内 dataflow BFS。

示例：

```ts
function wrap(x) {
  return sanitize(x)
}
```

summary：

```text
param x -> call_arg sanitize[0]
param x -> return
```

### 3.2 Callsite bridge

trace 时加入 virtual edges：

```text
caller CallArg[i] -> callee Parameter[i]
callee Return -> caller CallReturn
caller Receiver -> callee this/self
```

需要 resolver 提供 callee SymbolId；如果 resolver 不确定：

- 多候选
- confidence 降低
- diagnostics 标注 `low_confidence_resolution`

### 3.3 return value node

当前缺少稳定 `CallReturn` 节点。建议每个 call expression 生成：

```text
DataNodeKind::CallReturn
callsite_id = callsite.id
name = full call text
```

然后：

```text
CallTarget + CallArg -> CallReturn
CallReturn -> assigned local
```

跨函数：

```text
callee Return -> caller CallReturn
```

### 3.4 验收

每个语言都要有：

```text
source -> helper -> wrapper -> sink
```

例如：

```ts
function source(req) { return req.body.name }
function helper(x) { return sanitize(x) }
function controller(req) {
  const v = helper(source(req))
  sink(v)
}
```

trace `sink(v)` 必须跨函数到 `req.body.name`。

---

## 4. 非能力降级类问题：原因与解决方案

### 4.1 C feature 编译失败

#### 原因

`crates/atlas-engine/crates/extraction/src/languages/c.rs` 中：

```rust
impl DataflowSpec for CAdapter {
    ...
```

缺少关闭 `}`。

#### 影响

- `cargo check -p extraction --features c` 失败
- `all-languages` 可能失败
- 无法进入 C dataflow 验收

#### 解决方案

1. 补 brace。
2. 增加 feature matrix compile tests：

   ```bash
   cargo check -p extraction --no-default-features --features c
   cargo check -p atlas-cli --features all-languages
   ```

3. CI 增加每个语言单独 feature check。

### 4.2 JavaScript 单独 feature 编译失败

#### 原因

`javascript.rs` 依赖：

```rust
super::typescript::normalize_ts_...
```

但 `typescript` module 被：

```rust
#[cfg(feature = "typescript")]
```

gate 住。

#### 解决方案

短期：

```toml
javascript = ["typescript", "dep:tree-sitter-typescript"]
```

长期：

```text
languages/ecmascript_shared.rs
```

TS/JS/ArkTS 共用 normalize 和 query helpers。

### 4.3 extraction tests 没有按 feature 正确 cfg

#### 现象

`cargo test -p extraction dataflow` 在某些 feature 组合下出现：

```text
cannot find function ts_frontend
cannot find function py_frontend
```

#### 原因

测试函数调用了 `ts_frontend()` / `py_frontend()`，但测试本身没有对应：

```rust
#[cfg(feature = "typescript")]
#[cfg(feature = "python")]
```

#### 解决方案

对测试加 feature gate：

```rust
#[cfg(feature = "typescript")]
#[test]
fn test_extract_ts_simple() { ... }

#[cfg(feature = "python")]
#[test]
fn test_extract_python_simple() { ... }
```

并增加矩阵：

```bash
cargo test -p extraction --no-default-features --features typescript
cargo test -p extraction --no-default-features --features python
cargo test -p extraction --no-default-features --features java
```

### 4.4 capability 存在双重真相

#### 原因

现在有两套能力来源：

1. `LanguageCapabilityProfile`
2. frontend slot capability：

   ```rust
   frontend.dataflow.capability()
   frontend.lexical.capability()
   ```

index 阶段看 slot capability，trace 阶段看 profile capability。

#### 影响

可能出现：

- DB 里产生 dataflow facts；
- trace 却说 unsupported；
- 或 trace 说 supported；
- DB 里没有 dataflow facts。

#### 解决方案

建立单一真相：

```rust
LanguageFrontend::feature_matrix()
```

所有地方都用它：

- extraction 是否跑 dataflow
- extraction 是否跑 lexical
- trace 是否允许 trace_variable
- MCP language capabilities
- CLI status / doctor

即：

```rust
let features = frontend.feature_matrix();

if features.local_dataflow.is_supported() {
   run_dataflow()
}
```

不要一会儿用 slot capability，一会儿用 profile。

### 4.5 `ArgToParam` / `ReturnToCall` 语义错误

#### 原因

当前 intra-procedural builder 把：

```text
call_arg -> call_target
```

命名成 `ArgToParam`。

把：

```text
expr inside return -> return node
```

命名成 `ReturnToCall`。

这和 enum 注释的 interprocedural 语义不一致。

#### 解决方案

新增或重命名：

```rust
ArgToCall
ReturnValue
```

保留真正跨函数：

```rust
ArgToParam
ReturnToCall
ReceiverToThis
```

如果不想改 schema，可短期在 provenance 里修正描述，但长期建议改 enum。

### 4.6 `DataNodeId` 生成缺少 `function_id` 参与

#### 原因

当前很多 normalize 里：

```rust
DataNodeId::generate(
    &file_id,
    None::<&SymbolId>,
    ...
)
```

后续才补 `function_id`。

#### 问题

同一文件中 byte range 一般不会重复，因此短期可用；但从语义上看，DataNode ID 未包含 function，后续 function_id 改了但 id 不变。若未来引入增量片段解析、宏展开或虚拟节点，仅靠 file+byte 不够。

#### 解决方案

短期保持不动，避免大量 schema 震荡。

中期：

1. 先 resolve function ranges；
2. normalize 时传入 enclosing function_id；
3. DataNodeId 生成包含 function_id。

这需要调整 extraction pipeline 顺序：

```text
symbols/scopes -> function ranges -> lexical -> dataflow normalize
```

### 4.7 function range / body range 模型不足

#### 原因

当前 `SymbolDef.range` 有时是 name range，有时被扩成 function scope range，语义混乱。

#### 解决方案

最好扩展结构：

```rust
SymbolDef {
    range: TextRange,       // full declaration / definition
    name_range: TextRange,  // name only
    body_range: Option<TextRange>,
}
```

如果不改 schema，至少约定：

- function/method/constructor 的 `range` 必须是 full function range；
- name range 未来另存。

验收：

```text
所有 DataNode.function_id 都非 None，并等于所在函数。
```

### 4.8 callsite args 与 DataNode call args 关联不稳定

#### 原因

当前通过 provisional callsite id：

```rust
CallsiteId::from_file_byte(file_id, cs.range.start_byte)
```

再 rewrite 成 real callsite id。

这依赖：

- callsite range start_byte 完全一致；
- dataflow `find_call_expression` 找到的 call node 与 callsite extractor 找到的是同一个 node。

不同语言 AST 里不一定稳定。

#### 解决方案

统一 callsite extraction：

- dataflow 和 callsite 都用同一个 `CallsiteExtractorSpec`
- 先抽 callsites；
- 再 dataflow normalize 时通过 node range 查 callsite id；
- 不要先 provisional 再 rewrite。

推荐 pipeline：

```text
references
-> callsites with ranges
-> dataflow nodes, lookup callsite by enclosing range
```

### 4.9 统一 field/access_path 表示

#### 当前问题

不同语言 access path 不一致：

- TS: `req.body.name`
- Python: `req.body`
- C: `ptr->field`
- PHP: `$obj->field`
- Ruby: `params[:name]`
- Go: `req.Body.Name`

如果只是原文字符串，trace 很难跨语言解释。

#### 解决方案

保留原文，同时增加 normalized path：

```rust
AccessPath {
    raw: String,
    segments: Vec<AccessSegment>,
}

enum AccessSegment {
    Field(String),
    Index(String),
    PointerDeref,
    Optional,
    Static(String),
    This,
    Super,
}
```

短期可用 JSON 存在 `access_path` 字符串里，但建议最终结构化。

验收：

```text
每种语言字段访问 fixture 必须断言：
base variable -> field node
field node access_path 完整
```

---

## 5. 推荐实施顺序

### Phase 1：基础设施修正

1. 修 C 编译错误。
2. 修 JS feature gating。
3. 修 tests feature cfg。
4. 建立 language compile matrix。
5. 统一 capability 来源为 `feature_matrix()`。
6. 修 `Receiver` / `ArgToParam` / `ReturnToCall` 语义。
7. 改 `DataflowSpec`，支持 language-specific edge builder。
8. 建立 shared dataflow fixture harness。

验收：

```bash
cargo check -p atlas-cli --features all-languages
cargo test -p extraction --features all-languages
cargo test -p atlas-cli --features all-languages trace
```

### Phase 2：把 TS/JS/Python 做扎实

优先保证现有主线语言达到 L3/L5：

- TypeScript
- JavaScript
- Python

补：

- expression identifiers
- shadowing
- nested callsite
- field write
- destructuring / tuple unpack
- cross-function summary

### Phase 3：静态 OO 语言

然后做：

- Java
- C#
- Kotlin
- ArkTS

重点：

- property/field read/write
- constructor
- method receiver
- lambda
- expression body / await / safe call

### Phase 4：系统语言

然后做：

- C
- C++
- Rust
- Go

重点：

- pointer / reference
- multi-return
- pattern
- ownership / borrow
- macros
- templates

### Phase 5：动态语言

最后做：

- PHP
- Ruby
- Bash

重点：

- dynamic call diagnostics
- superglobal / external input
- implicit return
- hash/index access
- block/yield
- shell command substitution / pipes

---

## 6. 每个语言最低验收矩阵

每种正式支持 dataflow 的语言，至少要通过下面矩阵：

| 类别 | 必须有 fixture |
|---|---|
| 参数来源 | param -> local -> return |
| 表达式内部变量 | `result = a + b` |
| 简单赋值链 | `x = source; y = x; return y` |
| 字段读取 | `x = obj.field` |
| 字段写入 | `obj.field = x` |
| 索引读取 | `x = arr[i]` / `map[key]` |
| 调用实参 | `sink(x)` |
| 嵌套调用 | `foo(bar(a), b)` |
| 返回值 | `return x` |
| 本地 shadowing | inner/outer same name |
| 跨函数参数 | caller arg -> callee param |
| 跨函数返回 | callee return -> caller result |
| unsupported/low confidence | dynamic call / macro / overload 等 |

验收不能只断言“有 nodes/edges”，而要断言具体路径：

```rust
assert_trace_contains_chain([
  "result",
  "sanitize(name)",
  "name",
  "req.body.name",
  "req"
])
```

---

## 7. 总结

如果目标是下一轮真正把大部分语言推进到 dataflow 级别，核心不是继续补零散 `.scm`，而是先完成：

1. 语言专属 edge builder。
2. 表达式内部 identifier 分解。
3. scope-aware lexical binding。
4. 统一 `callsite_id`。
5. 明确 intra-procedural 和 inter-procedural edge 语义。
6. 稳定 function range / `function_id`。
7. 每语言 fixture-driven 验收。

逐语言优先级：

- TS/JS/Python：最快达到稳定 L3/L5。
- Java/C#/Kotlin/ArkTS：第二批，主要补 OO/member/lambda/constructor。
- C/C++/Rust/Go：第三批，主要补 pointer/reference/pattern/multi-return。
- PHP/Ruby：第四批，主要补动态语言语义和低置信 diagnostics。
- Bash/Cangjie：建议作为 experimental best-effort，最后做。

最重要的验收原则：

> 不能只看 data_nodes 数量，也不能只看 dataflow_edges 非空。必须对每个语言写 trace fixture，断言“从 sink 变量回溯到 source 参数/字段/返回值”的完整路径、edge kind、confidence 和 provenance。
