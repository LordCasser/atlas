# Atlas MVP 语言范围与实现计划

> MVP 语言已确定为：C / C++ / Python / Java / ArkTS / TypeScript / JavaScript / Cangjie。本文档定义这些语言的实现优先级、抽取能力、解析策略、限制和验收标准。

---

## 1. MVP 语言列表

```text
C
C++
Python
Java
ArkTS
TypeScript
JavaScript
Cangjie
```

用户曾写作 `Cnagjie`，本文统一为 **Cangjie / 仓颉**。

非 MVP / 后续路线：

```text
Go
Rust
C#
PHP
Ruby
Swift
Kotlin
Dart
Svelte
Vue
Liquid
Pascal
Scala
```

---

## 2. 为什么这个 MVP 组合需要重新设计架构

这 8 种语言覆盖三类语言模型：

### 2.1 脚本/动态语言

```text
Python
JavaScript
```

特点：

- 动态类型。
- 调用目标经常需要启发式解析。
- 构造调用和函数调用语法可能相同。

### 2.2 包/模块/类语言

```text
TypeScript
ArkTS
Java
```

特点：

- 类/接口/方法/import 比较清晰。
- 适合构建较高质量 call graph 和 type hierarchy。
- ArkTS MVP 可复用 TypeScript parser，但要保留语言标记。

### 2.3 系统/静态语言

```text
C
C++
Cangjie
```

特点：

- C/C++ 有 include、宏、头文件、namespace、模板、重载、函数指针等复杂点。
- Cangjie grammar 和生态集成需要 spike。
- MVP 应做 best-effort 静态图谱，不承诺编译器级精确性。

---

## 3. 推荐实现优先级

### Tier 1: 先打通核心闭环

```text
TypeScript
JavaScript
Python
Java
```

原因：

- tree-sitter grammar 成熟。
- 语言结构清晰。
- 可验证 extraction -> resolution -> graph -> MCP 的完整链路。

### Tier 2: 系统语言 best-effort

```text
C
C++
```

原因：

- 需要 include-aware resolution。
- 宏和模板不能在 MVP 做完整。
- 但 direct function/class/method/call/include graph 很有价值。

### Tier 3: Harmony / 新语言风险项

```text
ArkTS
Cangjie
```

策略：

- ArkTS：MVP 先用 TypeScript grammar 兜底。
- Cangjie：先做 grammar spike，再写 minimal adapter。

---

## 4. MVP 能力矩阵

| 能力 | C | C++ | Python | Java | ArkTS | TypeScript | JavaScript | Cangjie |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| file node | 必须 | 必须 | 必须 | 必须 | 必须 | 必须 | 必须 | 必须 |
| function extraction | 必须 | 必须 | 必须 | 必须 | 必须 | 必须 | 必须 | 必须 |
| class extraction | N/A | 必须 | 必须 | 必须 | 必须 | 必须 | 必须/部分 | 必须/部分 |
| struct extraction | 必须 | 必须 | N/A | N/A | 视语法 | TS 类型结构 | N/A | 必须/部分 |
| interface extraction | N/A | N/A | N/A | 必须 | 必须 | 必须 | N/A | 必须/部分 |
| method extraction | N/A/部分 | 必须 | 必须 | 必须 | 必须 | 必须 | 必须 | 必须/部分 |
| imports/includes | include | include/import | import/from | package/import | import | import/export | import/require/export | import |
| direct calls | 必须 | 必须 | 必须 | 必须 | 必须 | 必须 | 必须 | 必须 |
| member calls | 部分 | 必须 | 必须 | 必须 | 必须 | 必须 | 必须 | 必须/部分 |
| instantiation | N/A | new/ctor | Class() | new | new | new | new | 视语法 |
| inheritance | N/A | 必须 | 必须 | 必须 | 必须 | 必须 | 部分 | 必须/部分 |
| decorators/annotations | N/A | attrs 部分 | decorators | annotations | decorators | decorators | decorators | annotations? |
| type refs | 部分 | 部分 | 部分 | 必须 | 必须 | 必须 | 弱 | 部分 |
| local dataflow | 后续 | 后续 | 后续 | 后续 | 后续 | 后续 | 后续 | 后续 |
| framework resolver | 后续 | 后续 | 后续 | 后续 | 后续 | 后续 | 后续 | 后续 |

MVP 最重要闭环：

```text
symbols
contains
references
calls
imports/includes
extends/implements
instantiates
MCP query
```

---

## 5. TypeScript / JavaScript

### 5.1 Grammar

```text
tree-sitter-typescript
```

TypeScript：

```text
.ts
.tsx optional later; MVP 可先 .ts
```

JavaScript：

```text
.js
.mjs
.cjs
.jsx optional later; MVP 可先 .js/.mjs/.cjs
```

### 5.2 抽取目标

Definitions：

```text
function_declaration
arrow_function assigned to variable
function_expression assigned to variable
class_declaration
method_definition
public_field_definition arrow method
interface_declaration TS only
type_alias_declaration TS only
enum_declaration TS only
variable/constant top-level
```

Imports/exports：

```text
import_statement
export_statement
export ... from
require optional for JS
```

References：

```text
call_expression
new_expression
member_expression
identifier references optional
```

### 5.3 Resolution

支持：

```text
relative import
extension completion
index.ts / index.js
named import
default import
namespace import
re-export chain
path alias later or MVP basic
```

### 5.4 限制

MVP 不承诺：

```text
完整 TypeScript type checker
泛型实例化
复杂 dynamic import
JS runtime alias 精确追踪
```

---

## 6. ArkTS

### 6.1 MVP 策略

ArkTS 使用 `.ets` 后缀。

MVP 推荐：

```text
parser = TypeScript grammar fallback
language stored as arkts
adapter = TypeScriptAdapter with ArkTS tweaks
```

原因：

- 快速支持 HarmonyOS/ArkTS 常见类、函数、import、call。
- 避免被 ArkTS grammar 依赖风险阻塞。

### 6.2 后续策略

后续如果 TypeScript grammar 对 `.ets` 解析质量不足，再加入：

```text
tree-sitter-arkts
ArkTSNativeAdapter
```

可配置：

```toml
[atlas.languages.arkts]
parser = "typescript" # or "native"
```

### 6.3 MVP 抽取目标

```text
class
interface
function
method
field
import
export
call
new
decorator
```

### 6.4 限制

```text
ArkTS 特有语法可能解析失败或误解析
HarmonyOS framework resolver 后续再做
```

---

## 7. Python

### 7.1 Grammar

```text
tree-sitter-python
```

Extensions：

```text
.py
.pyw optional
.pyi optional later
```

### 7.2 抽取目标

Definitions：

```text
function_definition
class_definition
method = function_definition inside class
assignment top-level optional
```

Imports：

```text
import_statement
import_from_statement
aliased_import
relative import
```

References：

```text
call
attribute call
ClassName() as possible instantiation
```

Decorators：

```text
decorated_definition
decorator
```

### 7.3 Resolution

```text
same scope exact
same file exact
from x import y
import x as y
package __init__.py
relative import .foo
class constructor promotion
project-wide exact fallback
```

### 7.4 限制

MVP 不承诺：

```text
运行时 monkey patch
动态 import
类型推断
精确 instance method target
```

---

## 8. Java

### 8.1 Grammar

```text
tree-sitter-java
```

Extension：

```text
.java
```

### 8.2 抽取目标

Definitions：

```text
package_declaration
class_declaration
interface_declaration
enum_declaration
annotation_type_declaration
method_declaration
constructor_declaration
field_declaration
```

Imports：

```text
import_declaration
static import
wildcard import
```

References：

```text
method_invocation
object_creation_expression
field_access optional
identifier type refs
```

Inheritance：

```text
superclass
super_interfaces
extends_interfaces
```

Annotations：

```text
marker_annotation
annotation
```

### 8.3 Resolution

支持：

```text
package declaration
same package lookup
single type import
wildcard import
java.lang external filter
class-local method lookup
constructor resolution
```

Qualified name 建议：

```text
com.foo.Controller.login
```

### 8.4 限制

MVP 不承诺：

```text
完整 classpath
Maven/Gradle dependency resolution
method overload by signature 精确解析
泛型类型擦除/绑定
```

---

## 9. C

### 9.1 Grammar

```text
tree-sitter-c
```

Extensions：

```text
.c
.h 可先按 C；若检测到 C++ 特征再交给 C++ adapter
```

### 9.2 抽取目标

Definitions：

```text
function_definition
function prototype declaration optional
struct_specifier
enum_specifier
typedef_declaration
global variable optional
```

Imports/includes：

```text
preproc_include
#include "local.h"
#include <system.h>
```

References：

```text
call_expression
identifier callee
```

### 9.3 Resolution

```text
same file function exact
included local header symbols
project-wide exact fallback
system include external filter
```

### 9.4 限制

MVP 不承诺：

```text
宏展开
条件编译多配置
函数指针目标精确解析
链接器级跨 translation unit 解析
```

MCP/status 应提示：

```text
C analysis is preprocessor-light and best-effort.
```

---

## 10. C++

### 10.1 Grammar

```text
tree-sitter-cpp
```

Extensions：

```text
.cpp
.cc
.cxx
.hpp
.hh
.hxx
.h if detected C++
```

### 10.2 抽取目标

Definitions：

```text
namespace_definition
class_specifier
struct_specifier
function_definition
method definitions inside/outside class
constructor/destructor
field_declaration
template_declaration wrapper
```

Imports/includes：

```text
preproc_include
```

References：

```text
call_expression
qualified_identifier
scoped_identifier
field_expression
new_expression
```

Inheritance：

```text
base_class_clause
```

### 10.3 Resolution

```text
same class method lookup
namespace qualified lookup
include-aware lookup
same namespace exact
project-wide exact with proximity
constructor resolution
```

Confidence examples：

```text
exact qualified name: 0.95
same class method: 0.90
same namespace name: 0.80
include-proximity name: 0.70
global fuzzy: 0.50
```

### 10.4 限制

MVP 不承诺：

```text
模板实例化
重载 resolution 精确匹配
宏生成符号
operator overload 完整解析
ADL
virtual dispatch 精确 target
```

MCP/status 应提示：

```text
C++ analysis is best-effort without full compilation database semantics.
```

---

## 11. Cangjie / 仓颉

### 11.1 Grammar

当前 `Cargo.toml` 已有：

```toml
tree-sitter-cangjie = { git = "https://gitcode.com/Cangjie-SIG/tree-sitter-cangjie.git", optional = true }
```

Extensions：

```text
.cj
.cangjie
```

### 11.2 必须先做 grammar spike

在正式 adapter 之前，必须完成：

```text
1. 能否 cargo build 成功
2. tree-sitter language API 是否兼容当前 tree-sitter crate
3. AST node kinds 列表
4. function/class/import/call 的 AST shape
5. 最小 fixture 是否能稳定 parse
```

建议增加调试命令：

```text
atlas ast-dump <file.cj>
```

或测试工具函数输出 AST sexp。

### 11.3 MVP 抽取目标

```text
package/module
import
function
class/struct/interface
method
field
enum
call
inheritance/implements if grammar 支持
```

### 11.4 Resolution

先做：

```text
same file
same module/package
import exact
project-wide exact fallback
```

### 11.5 限制

```text
不承诺完整类型系统
不承诺复杂泛型/宏/编译器特性
不承诺 framework resolver
```

---

## 12. Cargo features 建议

当前默认 feature 应改为 MVP 语言：

```toml
[features]
default = ["mvp-languages", "cli", "mcp"]

mvp-languages = [
  "c",
  "cpp",
  "python",
  "java",
  "typescript",
  "javascript",
  "arkts",
  "cangjie",
]

typescript = ["dep:tree-sitter-typescript"]
javascript = ["dep:tree-sitter-typescript"]
arkts = ["typescript"]

python = ["dep:tree-sitter-python"]
java = ["dep:tree-sitter-java"]
c = ["dep:tree-sitter-c"]
cpp = ["dep:tree-sitter-cpp"]
cangjie = ["dep:tree-sitter-cangjie"]
```

后续如果有 native ArkTS grammar：

```toml
arkts-native = ["dep:tree-sitter-arkts"]
```

---

## 13. Language enum 和 extension mapping

MVP Language：

```rust
pub enum Language {
    C,
    Cpp,
    Python,
    Java,
    ArkTS,
    TypeScript,
    JavaScript,
    Cangjie,
}
```

如果保留其他语言 enum 也可以，但必须区分：

```text
supported in MVP
recognized but disabled
unknown
```

Extension mapping：

```text
.c       -> C
.h       -> C by default, Cpp if C++ heuristic matches
.cpp     -> Cpp
.cc      -> Cpp
.cxx     -> Cpp
.hpp     -> Cpp
.hh      -> Cpp
.hxx     -> Cpp
.py      -> Python
.java    -> Java
.ets     -> ArkTS
.ts      -> TypeScript
.js      -> JavaScript
.mjs     -> JavaScript
.cjs     -> JavaScript
.cj      -> Cangjie
.cangjie -> Cangjie
```

C++ header heuristic 可参考 CodeGraph：

```text
namespace
class
struct with access specifier
template <>
public/private/protected:
virtual
using namespace
```

---

## 14. MVP fixture 验收标准

每种语言至少 5 类 fixture：

```text
1. basic definitions
2. imports/includes
3. direct calls
4. class/method calls
5. inheritance/implements
```

### 14.1 TypeScript / ArkTS fixture

```ts
import { UserService } from "./service";

export class Controller {
  constructor(private svc: UserService) {}

  async login() {
    return this.svc.login();
  }
}
```

期望：

```text
file node created
Controller class symbol
login method symbol
Controller contains login
file imports ./service / UserService
login calls login or UserService.login with confidence
```

### 14.2 JavaScript fixture

```js
const { UserService } = require("./service");

class Controller {
  login() {
    const svc = new UserService();
    return svc.login();
  }
}
```

期望：

```text
Controller class
login method
login instantiates UserService
login calls login/UserService.login with confidence
require import captured if MVP supports require
```

### 14.3 Python fixture

```python
from service import UserService

class Controller:
    def login(self):
        svc = UserService()
        return svc.login()
```

期望：

```text
Controller class
login method
import UserService from service
login instantiates UserService
login calls login/UserService.login with lower confidence if receiver type unknown
```

### 14.4 Java fixture

```java
package app;

import service.UserService;

class Controller extends BaseController {
    UserService svc;

    void login() {
        svc.login();
    }
}
```

期望：

```text
package app stored
Controller class qualified as app.Controller
Controller extends BaseController
UserService import captured
login method
login calls login/UserService.login with confidence
```

### 14.5 C fixture

```c
#include "auth.h"

int login(char* user) {
    return check_user(user);
}
```

期望：

```text
file includes auth.h
login function
login calls check_user
check_user resolved if declared in included header or same project
```

### 14.6 C++ fixture

```cpp
#include "auth.hpp"

namespace app {
class Controller : public Base {
public:
    void login() {
        service.login();
    }
};
}
```

期望：

```text
namespace app
Controller class qualified app::Controller
Controller extends Base
login method
Controller contains login
login calls service.login/login with confidence
include auth.hpp captured
```

### 14.7 Cangjie fixture

根据实际 grammar 确定语法后补充。最小应覆盖：

```text
package/module
import
function
class or struct
method
call
```

期望：

```text
function/class/import/call can be extracted
same-module resolution works
```

---

## 15. MVP 验收总标准

MVP 完成时应满足：

1. 对 8 种语言能识别文件并 parse，Cangjie 至少有 grammar spike 结论。
2. 每种语言至少能抽取 file/function/class-or-struct/import-or-include/call。
3. `contains` 图可靠。
4. direct call graph 可用，低置信度边有标记。
5. import/include dependency graph 可用。
6. Java/TS/ArkTS/Python 的基础 import resolution 可用。
7. C/C++ 的 include-aware best-effort resolution 可用。
8. MCP 可查询 search/symbol/callers/callees/neighbors/impact/context/explore。
9. 所有 MCP 输出有长度限制和 confidence/provenance 信息。
10. 增量 sync 至少能 reindex changed files 并刷新 graph snapshot。

---

## 16. 最终语言策略结论

MVP 不做“全语言大而全”，而是：

```text
C / C++ / Python / Java / ArkTS / TypeScript / JavaScript / Cangjie
```

围绕这 8 种语言打通：

```text
symbols
scopes
references
imports/includes
calls
contains
type hierarchy where possible
MCP graph queries
```

实现时要接受语言差异：

- TS/JS/ArkTS：模块和前端生态友好，import/re-export 重要。
- Python：动态语言，resolution 需要 confidence。
- Java：package/import/class 层次清晰，质量应较高。
- C/C++：best-effort，include-aware，但不做完整编译器。
- Cangjie：先 grammar spike，再 minimal adapter。

这就是 Atlas Rust-native MVP 的语言边界。
