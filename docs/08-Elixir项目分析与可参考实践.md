# 08-Elixir 项目分析与可参考实践

> 本文记录对本地 `/Users/lordcasser/workspace/projects/elixir` 项目的架构分析结果，并明确哪些设计、文档、源码文件和工程实践值得 Atlas / Corpus 后续参考。本文服务于后续 `atlas-parse`、`apps/atlas`、`apps/atlas-corpus` 的架构和实现讨论，尤其是大型多版本源码语料库方向。

---

## 1. 分析对象与结论摘要

分析对象：

```text
/Users/lordcasser/workspace/projects/elixir
```

Elixir 项目是 Bootlin 开源的源码交叉引用系统。它的核心目标是：

```text
用较低存储成本索引 C/C++ 项目的每个 release，尤其是 Linux kernel，并提供 Web 源码浏览、identifier 定义/引用查询和 REST API。
```

Elixir 的核心设计可以概括为：

```text
Git tags 表示版本
Git bare repo 存储源码
Git blob hash 去重源码内容
Berkeley DB 存储索引
version -> blob/path 映射决定版本可见性
identifier -> blob/line postings 存储定义和引用
Web/API 层通过 project/version/path 查询源码和符号
```

对 Atlas / Corpus 最有价值的不是 Elixir 的 Python/Shell/Perl 实现本身，而是以下设计思想：

1. **Git tag 作为版本边界**。
2. **Git blob 作为索引去重单位**。
3. **只解析新 blob，不按版本重复解析相同文件内容**。
4. **全局 symbol/ref postings + version 文件集合过滤**。
5. **project/version/source/path 的稳定 Web URL 模型**。
6. **identifier 查询 API 简单稳定，适合人工和工具消费**。
7. **对 Linux 项目通过多个 remote 汇聚主线、stable 和历史版本 tags**。
8. **Linux 额外支持 Kconfig、Makefile、DTS、Devicetree compatible 等非普通 C 文件族**。

本文建议 Corpus 项目吸收这些设计，但不要照搬 Elixir 的实现技术栈。

---

## 2. Elixir 项目结构概览

本地 Elixir 根目录主要文件：

```text
README.adoc                         # 安装、架构、索引和 Web/API 使用说明
script.sh                           # Shell 底层服务：Git 访问、tag、blob、ctags、tokenize
update.py                           # 索引构建主程序
utils/index                         # Docker/生产环境索引入口脚本
projects/linux.sh                   # Linux 项目专用 tag 排序、菜单、DTS 支持配置
elixir/data.py                      # Berkeley DB 数据结构封装
elixir/query.py                     # 查询层：source、dir、versions、identifier
elixir/web.py                       # Web routes、HTML 页面生成、source/ident 页面
elixir/api.py                       # REST API：/api/ident
elixir/autocomplete.py              # /acp 自动补全
elixir/filters/                     # 源码 HTML 渲染后处理 filters
find_compatible_dts.py              # DTS compatible 字符串解析
find-file-doc-comments.pl           # 文档注释解析
```

建议 Corpus 后续重点阅读：

```text
README.adoc
utils/index
script.sh
update.py
projects/linux.sh
elixir/data.py
elixir/query.py
elixir/web.py
elixir/api.py
elixir/autocomplete.py
elixir/filters/
```

---

## 3. Elixir 总体架构

Elixir 的架构分三层：

```text
script.sh
  -> Git 和 Unix 工具访问层
  -> list-tags / get-file / get-dir / list-blobs / parse-defs / tokenize-file

update.py
  -> 索引构建层
  -> blob 去重、version index、definitions、references、docs、DTS compatible

elixir/query.py + web.py + api.py
  -> 查询和 Web/API 层
  -> source 浏览、identifier 查询、版本菜单、REST API、autocomplete
```

Elixir 通过环境变量将同一套代码绑定到不同 project：

```text
LXR_REPO_DIR   # 当前 project 的 bare Git repo
LXR_DATA_DIR   # 当前 project 的 Berkeley DB index data
LXR_PROJ_DIR   # Web 层所有 project 的父目录
```

Web 侧项目目录约定：

```text
<LXR_PROJ_DIR>/
  linux/
    repo/
    data/
  u-boot/
    repo/
    data/
  busybox/
    repo/
    data/
```

对 Corpus 的启发：

```text
/srv/atlas-data/
  linux/
    repo/
    corpus.db
    index/
  u-boot/
    repo/
    corpus.db
    index/
```

Corpus 应保留这种 project-level 数据隔离方式，但内部存储不必使用 Berkeley DB。

---

## 4. 多版本访问机制分析

### 4.1 版本来源：Git tags

Elixir 默认通过 `git tag` 获取版本列表。底层逻辑在 `script.sh`：

```sh
get_tags()
{
    git tag |
    version_dir |
    sed 's/$/.0/' |
    sort -V |
    sed 's/\.0$//'
}
```

Linux 项目对 tag 排序和菜单做了项目级覆盖，位于：

```text
projects/linux.sh
```

Linux 专用配置：

```sh
# Enable DT bindings compatible strings support
dts_comp_support=1

get_tags()
{
    git tag |
    version_dir |
    sed -r '...' |
    sort -V |
    sed -r '...'
}

list_tags_h()
{
    echo "$tags" |
    tac |
    sed -r '...'
}
```

作用：

1. 将 Linux 复杂 tag 名称按版本语义排序。
2. 生成 Web 版本菜单需要的层级结构。
3. 启用 Linux DTS compatible 额外索引。

Corpus 可参考：

```text
需要为不同 corpus project 提供 version/tag normalization 和 menu grouping 策略。
Linux 应有独立 VersionPolicy，而不是通用字典序排序。
```

建议 Corpus 设计：

```rust
pub trait VersionPolicy {
    fn normalize_tag(&self, raw: &str) -> Option<String>;
    fn sort_tags(&self, tags: &mut [CorpusTag]);
    fn group_for_menu(&self, tag: &str) -> VersionMenuPath;
    fn latest_stable<'a>(&self, tags: &'a [CorpusTag]) -> Option<&'a CorpusTag>;
    fn latest_rc<'a>(&self, tags: &'a [CorpusTag]) -> Option<&'a CorpusTag>;
}
```

---

### 4.2 Linux 多 remote 策略

Elixir README 和 `utils/index` 都体现了 Linux 多 remote 策略。

`utils/index` 中 Linux 默认 remote：

```bash
add_default_remotes $1 $# $2 linux \
  https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git \
  https://git.kernel.org/pub/scm/linux/kernel/git/stable/linux.git \
  https://github.com/bootlin/linux-history.git
```

目的：

1. 主线仓库提供主要 release 和 rc tags。
2. stable 仓库提供稳定版 patch releases。
3. linux-history 补充旧历史版本。

Corpus 可参考：

```text
Linux corpus 不应只依赖单个 remote。
应支持一个 project 绑定多个 remote，并 fetch --all --tags。
```

建议 Corpus project config：

```toml
[project]
name = "linux"
type = "linux-kernel"

[[remotes]]
name = "torvalds"
url = "https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git"

[[remotes]]
name = "stable"
url = "https://git.kernel.org/pub/scm/linux/kernel/git/stable/linux.git"

[[remotes]]
name = "history"
url = "https://github.com/bootlin/linux-history.git"
```

---

### 4.3 源码访问：不 checkout，直接访问 Git object

Elixir 访问源码文件时，不为每个版本 checkout 一份源码，而是直接使用 Git object：

```sh
get_file()
{
    v=`echo $opt1 | version_rev`
    git cat-file blob "$v:`denormalize $opt2`" 2>/dev/null
}
```

目录访问：

```sh
get_dir()
{
    v=`echo $opt1 | version_rev`
    git ls-tree -l "$v:`denormalize $opt2`" 2>/dev/null |
    awk '{print $2" "$5" "$4" "$1}' |
    grep -v ' \.' |
    sort -t ' ' -k 1,1r -k 2,2
}
```

文件类型判断：

```sh
get_type()
{
    v=`echo $opt1 | version_rev`
    git cat-file -t "$v:`denormalize $opt2`" 2>/dev/null
}
```

Web URL：

```text
/linux/v6.8/source/kernel/sched/core.c
```

底层等价于：

```text
git cat-file blob "v6.8:kernel/sched/core.c"
```

Corpus 应继承这个原则：

```text
不要为每个版本维护 checkout 目录。
使用 bare repo + tag/path + blob oid 访问源码。
```

---

## 5. Elixir 索引构建机制分析

### 5.1 索引入口：utils/index

`utils/index` 是生产/Docker 场景的索引入口，负责：

1. 初始化 project 目录。
2. 初始化 bare Git repo。
3. 添加默认 remote。
4. fetch remotes/tags。
5. 调用 `update.py` 建索引。

关键函数：

```bash
project_init()
project_add_remote()
project_fetch()
project_index()
do_index()
```

`project_index()`：

```bash
LXR_REPO_DIR=$1/repo LXR_DATA_DIR=$1/data \
    python3 "$elixir_sources/update.py" $ELIXIR_THREADS
```

首次索引时执行两轮 fetch/index：

```bash
project_fetch "$1"
project_index "$1"
project_fetch "$1"
project_index "$1"
```

原因：全量索引耗时较长，索引过程中 upstream 可能发布新 tag，第二轮用于补齐。

Corpus 可参考：

```text
提供 corpus init / remote add / sync / index 命令。
首次大规模索引可在完成后再次 sync/index 补齐新 tags。
```

---

### 5.2 update.py 主流程

`update.py` 是索引构建核心。

主流程：

```text
open DB
read tag list
filter tags not indexed in versions.db
start threads:
  UpdateIds
  UpdateVersions
  UpdateDefs
  UpdateRefs
  UpdateDocs
  UpdateComps
  UpdateCompsDocs
join threads
```

只处理未索引 tag：

```python
for tag in scriptLines('list-tags'):
    if not db.vers.exists(tag):
        tag_buf.append(tag)
```

对 Corpus 的启发：

```text
Corpus index 必须是增量的。
已经完成 version index 的 tag 不应重复索引。
已经解析过的 blob 不应重复解析。
```

---

### 5.3 Blob 去重：Elixir 最重要设计

`UpdateIds.update_blob_ids()` 对每个 tag 遍历所有 blob：

```python
blobs = scriptLines('list-blobs', '-f', tag)
```

底层：

```sh
git ls-tree -r "$v"
```

如果 blob hash 不存在，则分配新的内部 idx：

```python
blob_exist = db.blob.exists(hash)
if not blob_exist:
    db.blob.put(hash, idx)
    db.hash.put(idx, hash)
    db.file.put(idx, filename)
```

数据库关系：

```text
blobs.db:     git_blob_hash -> internal_blob_id
hashes.db:    internal_blob_id -> git_blob_hash
filenames.db: internal_blob_id -> representative filename
```

这是 Elixir 节省空间和索引时间的核心。

Corpus 必须继承：

```text
project + git_blob_oid -> BlobId
只解析新 BlobId
多版本共享同一个 BlobId 的解析结果
```

建议 Corpus 表：

```sql
corpus_blobs (
  blob_id INTEGER PRIMARY KEY,
  project_id TEXT NOT NULL,
  git_oid TEXT NOT NULL,
  size INTEGER NOT NULL,
  family TEXT,
  representative_path TEXT,
  indexed INTEGER NOT NULL DEFAULT 0,
  indexed_at INTEGER,
  UNIQUE(project_id, git_oid)
);
```

---

### 5.4 version -> blob/path 映射

`UpdateVersions.update_versions()` 对每个 tag 记录该版本文件集合：

```python
blobs = scriptLines('list-blobs', '-p', tag)
for blob in blobs:
    hash, path = blob.split(b' ', maxsplit=1)
    idx = db.blob.get(hash)
    buf.append((idx, path))
...
db.vers.put(tag, obj, sync=True)
```

`PathList` 存储：

```python
class PathList:
    '''Stores associations between a blob ID and a file path.'''
```

结果类似：

```text
versions.db:
  v6.8 -> [
    123 kernel/sched/core.c
    456 mm/memory.c
    789 include/linux/sched.h
  ]
```

Corpus 必须有同等结构：

```sql
corpus_version_files (
  id INTEGER PRIMARY KEY,
  project_id TEXT NOT NULL,
  tag_name TEXT NOT NULL,
  path TEXT NOT NULL,
  blob_id INTEGER NOT NULL,
  mode TEXT,
  file_size INTEGER,
  UNIQUE(project_id, tag_name, path)
);
```

查询源码：

```text
project + tag + path -> blob_id -> git_oid -> blob content
```

查询符号：

```text
symbol -> blob postings
version -> blob set
intersect
blob_id -> path in version
```

---

### 5.5 definitions 索引

Elixir 对新 blob 解析定义：

```python
family = lib.getFileFamily(filename)
if family in [None, 'M']: continue
lines = scriptLines('parse-defs', hash, filename, family)
```

C/ASM 定义使用 ctags，并额外解析 Linux 宏：

```sh
ctags -x --kinds-c=+p+x --extras='-{anonymous}' "$full_path"
perl -ne '/^\s*ENTRY\((\w+)\)/ and print "$1 function $.\n"'
perl -ne '/^SYSCALL_DEFINE[0-9]\(\s*(\w+)\W/ and print "sys_$1 function $.\n"'
```

Kconfig：

```sh
ctags -x --language-force=kconfig --kinds-kconfig=c ... |
awk '{print "CONFIG_"$1" "$2" "$3}'
```

DTS：

```sh
ctags -x --language-force=dts "$full_path"
```

定义写入：

```text
definitions.db:
  ident -> DefList(blob_id, type, line, family)
```

Corpus 可参考但应改进：

1. 不建议继续依赖 ctags 作为唯一解析器。
2. 可通过 `atlas-parse` tree-sitter 提供函数/结构体/方法/范围。
3. Linux-specific 宏可放入 `atlas-lang-linux`。
4. Corpus 应额外保存函数范围和 body hash，这是 Elixir 缺失但 AI Agent 非常需要的能力。

建议 Corpus definitions：

```text
blob_id + local symbol -> name/kind/range/signature/function body range
symbol name -> postings(blob_id, line, kind, family)
```

---

### 5.6 references 索引

Elixir references 的逻辑：

1. 对文件 tokenize。
2. 只把已经存在于 `definitions.db` 中的 token 当作 reference。
3. 避免把定义行自身计为引用。
4. Makefile 中只索引 `CONFIG_*`。

核心逻辑：

```python
tokens = scriptLines('tokenize-file', '-b', hash, family)
...
if db.defs.exists(tok) and not definition_at_same_line:
    idents[tok] += line_num
```

优点：

```text
简单、快速、适合源码交叉引用。
```

不足：

```text
不是真正语义级引用解析。
函数指针、宏展开、类型上下文、作用域解析都很弱。
```

Corpus 参考建议：

1. MVP 可采用类似思想：先建立 definition dictionary，再根据 token/reference occurrence 建立引用。
2. 对 C/C++ 可以结合 tree-sitter call_expression 输出，提高 callsite 准确度。
3. 保留 reference occurrence，不只存最终 edge。
4. 对大型 symbol postings 使用压缩结构或 bitmap 加速。

---

### 5.7 docs 和 DTS compatible 索引

Elixir 支持：

```text
doccomments.db
compatibledts.db
compatibledts_docs.db
```

Linux 项目通过 `projects/linux.sh` 启用：

```sh
dts_comp_support=1
```

DTS compatible 的查询模型很有参考价值：

```text
C files      定义/声明 compatible table
DTS/DTSI     使用 compatible string
bindings doc 文档化 compatible string
```

对 Linux bug/CVE 分析很有价值，尤其是驱动和设备树相关问题。

Corpus 建议把 DTS compatible 作为 Linux-specific extension：

```text
atlas-lang-linux/dts.rs
atlas-lang-linux/compatible.rs
```

MCP 可提供：

```text
corpus_search_compatible
corpus_compatible_usage
corpus_compatible_docs
```

---

## 6. Elixir 查询机制分析

### 6.1 Query 对象

`elixir/query.py` 中 `get_query()` 根据 project 找到：

```text
basedir/project/data
basedir/project/repo
```

然后构造：

```python
Query(data_dir, repo_dir)
```

Query 每次调用 `script.sh` 时注入：

```python
"LXR_REPO_DIR": self.repo_dir,
"LXR_DATA_DIR": self.data_dir
```

Corpus 可参考：

```text
Web/API/MCP 查询应统一通过 CorpusQuery service，不直接访问底层 DB/Git。
```

建议接口：

```rust
pub trait CorpusQuery {
    fn list_versions(&self, project: &str) -> Result<Vec<VersionInfo>>;
    fn latest_version(&self, project: &str, include_rc: bool) -> Result<String>;
    fn get_source(&self, project: &str, version: &str, path: &str) -> Result<SourceFile>;
    fn get_dir(&self, project: &str, version: &str, path: &str) -> Result<Vec<DirEntry>>;
    fn search_ident(&self, project: &str, version: &str, family: Family, ident: &str) -> Result<IdentResult>;
    fn get_function(&self, project: &str, version: &str, symbol: &str) -> Result<FunctionSource>;
}
```

---

### 6.2 identifier 查询：version 过滤全局 postings

Elixir 的 `get_idents_defs()` 是理解其索引模型的关键。

简化流程：

```python
files_this_version = self.db.vers.get(version).iter()
defs_this_ident = self.db.defs.get(ident).iter(dummy=True)
refs = self.db.refs.get(ident).iter(dummy=True)
docs = self.db.docs.get(ident).iter(dummy=True)

for file_idx, file_path in files_this_version:
    advance defs/refs/docs to current file_idx
    if def_idx == file_idx:
        append definition result
    if ref_idx == file_idx:
        append reference result
```

关键思想：

```text
identifier postings 是全局 blob 维度。
version 文件列表决定当前版本可见哪些 blob。
查询时对二者做交集。
```

Corpus 必须继承这个思想，但实现上可更高效：

```text
Elixir:
  sorted list merge

Corpus:
  version roaring bitmap ∩ symbol postings
```

建议：

```text
version_bitmap(project, tag) -> set(blob_id)
symbol_defs(project, family, ident) -> postings sorted by blob_id
symbol_refs(project, family, ident) -> postings sorted by blob_id
```

查询：

```text
visible_blob_ids = version_bitmap(tag)
for posting in symbol postings:
  if visible_blob_ids.contains(posting.blob_id):
    path = version_path(tag, posting.blob_id)
    return path + lines
```

---

### 6.3 latest/latest-rc

Elixir 对 `latest` 和 `latest-rc` 做重定向。

Web 逻辑：

```python
if version in ('latest', 'latest-rc'):
    rc = version == 'latest-rc'
    version = query.get_latest_tag(rc=rc)
    resp.status = falcon.HTTP_FOUND
    resp.location = stringify_source_path(project, version, path)
```

Query 逻辑：

```python
def get_latest_tag(self, rc):
    if rc:
        sorted_tags = list(reversed(self.scriptLines('list-tags')))
    else:
        sorted_tags = self.scriptLines('get-latest-tags')

    for tag in sorted_tags:
        if self.db.vers.exists(tag):
            return tag.decode()
```

Corpus 应兼容：

```text
/{project}/latest/source/...
/{project}/latest-rc/source/...
```

并保证只返回已索引版本。

---

## 7. Web/API 兼容分析

### 7.1 Elixir Web routes

`elixir/web.py` 中核心 routes：

```python
app.add_route('/', IndexResource())
app.add_route('/{project}/{version}/source/{path:path}', SourceResource())
app.add_route('/{project}/{version}/source', SourceWithoutPathResource())
app.add_route('/{project}/{version}/ident', IdentPostRedirectResource())
app.add_route('/{project}/{version}/ident/{ident}', IdentWithoutFamilyResource())
app.add_route('/{project}/{version}/{family}/ident/{ident}', IdentResource())
app.add_route('/acp', AutocompleteResource())
app.add_route('/api/ident/{project:project}/{ident:ident}', ApiIdentGetterResource())
```

Corpus 若要 Elixir-compatible，应实现：

```text
GET /
GET /{project}/{version}/source
GET /{project}/{version}/source/{path}
GET /{project}/{version}/source/{path}?raw=1
GET /{project}/{version}/ident
GET /{project}/{version}/ident/{ident}
GET /{project}/{version}/{family}/ident/{ident}
GET /acp?q={prefix}&p={project}&f={family}
GET /api/ident/{project}/{ident}?version={version}&family={family}
```

---

### 7.2 REST API：/api/ident

`elixir/api.py` 中 `/api/ident` 返回结构：

```json
{
  "definitions": [
    {"path": "...", "line": 123, "type": "function"}
  ],
  "references": [
    {"path": "...", "line": "12,15", "type": null}
  ],
  "documentations": [
    {"path": "...", "line": 88, "type": null}
  ]
}
```

注意兼容细节：

1. `line` 有时是 number，有时是逗号分隔 string。
2. `type` 可能是 null。
3. `version=latest` 需要解析到已索引 latest。
4. family 不合法时默认 C。

Rust 兼容结构可设计：

```rust
#[derive(Serialize)]
#[serde(untagged)]
pub enum LineValue {
    Single(u32),
    Multiple(String),
}

#[derive(Serialize)]
pub struct SymbolInstance {
    pub path: String,
    pub line: LineValue,
    #[serde(rename = "type")]
    pub typ: Option<String>,
}

#[derive(Serialize)]
pub struct IdentResponse {
    pub definitions: Vec<SymbolInstance>,
    pub references: Vec<SymbolInstance>,
    pub documentations: Vec<SymbolInstance>,
}
```

---

### 7.3 Autocomplete API

Elixir `/acp`：

```text
GET /acp?q={prefix}&f={family}&p={project}
```

返回：

```json
["schedule", "scheduler_tick", "..."]
```

Elixir 使用 Berkeley DB cursor `DB_SET_RANGE` 做 prefix scan。

Corpus 可用：

```text
SQLite index prefix query
RocksDB/redb prefix scan
symbol dictionary trie/FST
```

MVP 可用 SQLite：

```sql
SELECT name
FROM corpus_symbol_dictionary
WHERE project_id = ? AND family = ? AND name >= ? AND name < ?
ORDER BY name
LIMIT 10;
```

后续可替换为 FST 或 segment dictionary。

---

## 8. Elixir 文件族与语言支持分析

Elixir 的 `lib.getFileFamily()` 支持：

```text
C: .c .cc .cpp .c++ .cxx .h .s
D: .dts .dtsi
K: Kconfig*
M: Makefile* .mk
```

文件族兼容关系：

```text
C query: C + K
K query: K
D query: D + C macros
M query: K
```

对 Corpus 的启发：

```text
Corpus 不应只索引 C 文件。
Linux 分析需要 C / headers / asm / Kconfig / Makefile / DTS / DTSI / binding docs。
```

建议 Corpus family：

```rust
pub enum CorpusFamily {
    All,
    C,
    Cpp,
    Asm,
    Kconfig,
    Makefile,
    Devicetree,
    DevicetreeBinding,
    MarkdownOrDocs,
}
```

但对 Elixir API 兼容时仍保留：

```text
A B C D K M
```

其中：

```text
A = all
B = DT binding compatible docs
C = C/C++/ASM
D = DTS/DTSI
K = Kconfig
M = Makefile
```

---

## 9. Elixir filters 分析

`elixir/filters/` 用于源码渲染后的 HTML 后处理。

典型能力：

```text
C include 链接
DTS include 链接
Makefile 文件/目录链接
Kconfig identifier 链接
DTS compatible 字符串链接
```

值得参考的 filters：

```text
cppinc.py              # #include "file"
cpppathinc.py          # #include <file>
dtsi.py                # /include/ "file"
dtscompcode.py         # C 代码中的 .compatible = "..."
dtscompdts.py          # DTS compatible = "..."
dtscompdocs.py         # bindings 文档中的 compatible
kconfig.py             # Kconfig 链接
makefiledir.py         # Makefile 目录链接
makefilefile.py        # Makefile 文件路径链接
makefiledtb.py         # dtb -> dts 链接
makefilesrctree.py     # $(srctree)/... 链接
```

Corpus Web 如需人工浏览体验，应参考这些 filter 的产品能力，而不是逐行迁移 Python 实现。

Rust 侧建议：

```text
raw source
  -> tokenize/annotate
  -> syntax highlight
  -> linkify identifiers/includes/configs/compatible
  -> line anchors
```

可用技术：

```text
syntect 或 tree-sitter-highlight
minijinja/tera templates
rust-embed static assets
```

---

## 10. Elixir 可参考文档与源码清单

### 10.1 一级参考：必须重点参考

| 文件 | 参考价值 | 建议用途 |
|---|---|---|
| `README.adoc` | 高 | 理解 Elixir 安装、架构、Linux 索引、Web/API、Docker、维护方式 |
| `utils/index` | 高 | 参考 corpus CLI、remote 管理、fetch/index 流程 |
| `script.sh` | 高 | 参考 Git tag/blob/tree 操作、file family、ctags/tokenize 设计 |
| `update.py` | 高 | 参考 blob 去重、version index、defs/refs/docs/comps 索引 pipeline |
| `projects/linux.sh` | 高 | 参考 Linux tag 排序、版本菜单、DTS compatible 开关 |
| `elixir/data.py` | 高 | 参考数据结构：DefList、PathList、RefList、DB 文件划分 |
| `elixir/query.py` | 高 | 参考 version filtering、identifier 查询、source/dir/latest 逻辑 |
| `elixir/web.py` | 高 | 参考 Elixir-compatible routes、source/ident 页面行为 |
| `elixir/api.py` | 高 | 参考 `/api/ident` JSON 兼容格式 |
| `elixir/autocomplete.py` | 高 | 参考 `/acp` prefix autocomplete 行为 |

---

### 10.2 二级参考：按功能选择参考

| 文件/目录 | 参考价值 | 建议用途 |
|---|---|---|
| `elixir/filters/` | 中高 | 参考源码 HTML linkify 功能和 Linux-specific Web 体验 |
| `find_compatible_dts.py` | 中高 | 参考 DTS compatible 字符串识别逻辑 |
| `find-file-doc-comments.pl` | 中 | 参考 kernel-doc/doc comment 关联思路 |
| `templates/` | 中 | 参考页面信息结构，但注意 AGPLv3 许可证 |
| `static/` | 中 | 参考 UI 行为，但注意 AGPLv3 许可证 |
| `docker/Dockerfile` | 中 | 参考部署依赖、ctags 构建、Apache/WSGI 部署方式；Rust 版不应照搬 |
| `docker/000-default.conf` | 低中 | 仅用于理解旧 Web 部署方式 |
| `t/` | 中 | 参考测试场景，尤其是 C/ASM/syscall/doc comment 边界问题 |

---

### 10.3 只建议理解，不建议照搬

| 文件/机制 | 原因 |
|---|---|
| Berkeley DB 存储方式 | Rust Corpus 应使用 SQLite metadata + bitmap/postings/segment，不建议继续 Berkeley DB |
| Python + Shell + Perl 多进程 pipeline | 可理解流程，但 Rust 应减少 fork/exec，内置 parser/tokenizer |
| Pygments HTML 高亮 | Rust 可用 syntect/tree-sitter-highlight 替代 |
| Apache + mod_wsgi 部署 | Rust 应提供单二进制 Web/MCP 服务 |
| Jinja templates 直接复制 | 可能涉及 AGPLv3 许可证，需要单独决策 |
| ctags 作为唯一 C 解析来源 | Corpus 应优先复用 `atlas-parse` tree-sitter，并用 Linux-specific rules 补充 |

---

## 11. 对 Atlas / Corpus 的具体参考建议

### 11.1 `atlas-ir` / `atlas-parse` 可参考点

从 Elixir 学到：

```text
需要提取 definitions、references、doc comments、file family。
需要保留 line range。
需要考虑 C/Kconfig/DTS/Makefile 不同 family。
需要对 Linux 特殊宏进行额外识别。
```

但 `atlas-parse` 不应包含：

```text
Git tag/blob/version 逻辑
Berkeley DB 风格 postings
Web/API 兼容逻辑
```

---

### 11.2 Atlas 原始项目可参考点

Atlas 原始项目是单项目单版本图谱，不需要复制 Elixir 的多版本模型。

可借鉴：

```text
C/Kconfig/DTS/Makefile family 思路
源码 linkify/filter 的产品体验
identifier 查询结果的人类可读组织方式
```

不建议引入：

```text
Git blob 去重
version -> blob/path 映射
Elixir-compatible routes
Linux 多 remote 管理
```

这些属于 Corpus。

---

### 11.3 Corpus 项目必须参考点

Corpus 应重点继承：

```text
Git tag = version
bare repo 存储源码
多 remote fetch tags
Git blob hash 去重
只解析新 blob
version -> blob/path 映射
identifier postings 按 blob 存储
查询时用 version 文件集合过滤 postings
latest/latest-rc 行为
/source 和 /ident URL 模型
/api/ident JSON 结构
/acp autocomplete
DTS compatible 支持
```

Corpus 应改进：

```text
使用 Rust-native parser/extraction
保存函数范围和函数体 hash
提供 MCP first-seen / diff / function timeline tools
使用 roaring bitmap 和 compressed postings 加速查询
使用单二进制 Web/MCP 服务
```

---

## 12. Corpus 建议 API 兼容范围

### 12.1 Web URL 兼容

应兼容：

```text
/{project}/{version}/source
/{project}/{version}/source/{path}
/{project}/{version}/source/{path}?raw=1
/{project}/{version}/ident
/{project}/{version}/ident/{ident}
/{project}/{version}/{family}/ident/{ident}
```

### 12.2 REST API 兼容

应兼容：

```text
/api/ident/{project}/{ident}?version={version}&family={family}
/acp?q={prefix}&p={project}&f={family}
```

### 12.3 行为兼容

应兼容：

```text
version = latest
version = latest-rc
family invalid -> default C
source raw mode
identifier not found status
path validation
```

---

## 13. Corpus 建议 MCP 工具

Elixir 本身没有 MCP，但其查询模型可以直接转化为 MCP tools。

建议工具：

```text
corpus_list_versions
corpus_get_source
corpus_get_dir
corpus_search_ident
corpus_autocomplete
corpus_get_function
corpus_diff_function
corpus_symbol_first_seen
corpus_function_timeline
corpus_git_blame
corpus_git_pickaxe
corpus_search_compatible
```

这些工具应基于 Elixir 的源码/identifier 查询基础，增加 AI Agent 分析 bug/CVE 所需的函数级和版本级能力。

---

## 14. 与 Elixir 相比 Corpus 应增强的能力

### 14.1 函数范围索引

Elixir 主要保存定义行，不完整保存函数体范围。

Corpus 应保存：

```text
function start_line
function end_line
start_byte
end_byte
signature
body range
```

用于：

```text
get_function
function diff
function body first-seen
context extraction
```

---

### 14.2 函数体 hash

Corpus 应为函数保存：

```text
raw_body_hash
normalized_body_hash
```

用途：

```text
判断某个函数实现在哪些版本相同
定位函数实现首次出现版本
生成函数变化 timeline
```

---

### 14.3 version bitmap

Elixir 使用 sorted list merge。Corpus 可使用 roaring bitmap：

```text
version -> bitmap(blob_id)
symbol -> postings(blob_id, lines, kind)
query = postings ∩ bitmap
```

这对 Linux 多版本查询会更快。

---

### 14.4 Agent-oriented 输出预算

Elixir Web/API 面向人工和普通 HTTP 客户端。Corpus MCP 必须面向 LLM 上下文预算：

```text
max_results
max_lines
max_bytes
truncated flag
summary + structuredContent
source URL references
```

---

### 14.5 Commit-level 辅助

Elixir 主要是 release/tag 级源码浏览。Corpus 可增加：

```text
git blame
git log -S / -G pickaxe
commit between versions
```

但应明确：

```text
release first-seen != commit introduced-by
```

MVP 可先做 release first-seen，commit-level 作为辅助候选。

---

## 15. 风险与约束

### 15.1 许可证风险

Elixir 是 AGPLv3。Atlas 当前是 MIT。

风险点：

```text
直接复制 templates/static/css/js/images 可能引入 AGPLv3 义务。
```

建议：

```text
优先兼容 URL/API 行为。
前端资源是否复用需要单独许可证决策。
如希望保持 MIT，应重新实现前端或仅参考交互结构。
```

---

### 15.2 不要照搬技术栈

Elixir 使用：

```text
Python
Shell
Perl
Universal Ctags
Berkeley DB
Falcon/mod_wsgi/Apache
Pygments
```

Corpus 应使用 Rust-native：

```text
atlas-parse/tree-sitter
SQLite metadata
roaring bitmap
compressed postings / segment files
axum or equivalent Web server
rust-embed
syntect/tree-sitter-highlight
MCP stdio/http
```

---

### 15.3 C/C++ 精度预期

Elixir 本身也不是编译器级 C 语义分析。Corpus 不应承诺：

```text
完整 preprocessing
完整宏展开
函数指针精确解析
C++ overload/template 精确解析
```

应定位为：

```text
源码级 cross-reference + function-level version analysis + best-effort semantic facts
```

---

## 16. 推荐后续文档

本文是 Elixir 分析和可参考实践文档。后续如推进 Corpus 项目，建议新增：

```text
09-Corpus需求规格.md
10-Corpus索引架构.md
11-Elixir兼容WebAPI.md
12-Corpus-MCP工具规格.md
13-Corpus数据模型.md
14-Corpus实施计划.md
```

这些文档应基于本文中的 Elixir 分析结果，但结合 Atlas workspace split 决策，确保 `atlas-parse` 与 Corpus 后端职责分离。

---

## 17. 最终建议

Elixir 对 Corpus 最重要的启发是：

```text
用 Git tag 表示版本，
用 Git blob 去重源码内容，
只解析新 blob，
用 version -> blob/path 映射表达版本文件集合，
用 identifier -> blob postings 表达定义和引用，
查询时按 version 过滤 postings。
```

Corpus 应继承这些核心思想，同时使用 Rust 和 Atlas 解析核心进行升级：

```text
Elixir-inspired index model
  + atlas-parse tree-sitter extraction
  + function range/body hash
  + version bitmap/posting acceleration
  + Elixir-compatible Web/API
  + Agent-native MCP tools
```

Atlas 原始项目则不应承担 Elixir 多版本索引职责。正确方向是：

```text
atlas-ir / atlas-parse 共享解析能力；
apps/atlas 继续做单项目单版本图谱；
apps/atlas-corpus 独立实现大型多版本源码索引。
```
