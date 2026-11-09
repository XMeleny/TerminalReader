# xmread

在终端中阅读 TXT、EPUB 和无 DRM 的 MOBI 纯文字内容。当前在 Windows PowerShell 验证；格式、分页和状态模块不依赖终端平台。

## 构建与运行

需要 Rust 工具链。在 PowerShell 中运行：

```powershell
cargo build --release
.\target\release\xmread.exe "D:\books\example.epub"
.\target\release\xmread.exe "D:\books\example.epub" --chap 12
.\target\release\xmread.exe
```

不带路径时打开最近阅读的书。`--chap` 从 1 开始编号，并从指定章首打开。

## 操作

| 按键或命令 | 行为 |
| --- | --- |
| `Left` / `k` | 前一页 |
| `Right` / `j` | 后一页 |
| `Ctrl+Left` / `Ctrl+k` | 上一章 |
| `Ctrl+Right` / `Ctrl+j` | 下一章 |
| `/toc`、`/toc 12` | 目录或跳章 |
| `/search 关键词` | 全文搜索 |
| `/bookmark add [名称]` | 添加书签，名称可省略 |
| `/bookmark`、`/bookmark delete 编号` | 查看或删除书签 |
| `/encoding`、`/encoding GBK` | 查看或指定 TXT 编码 |
| `/quit` | 退出 |

输入 `/` 后可查看命令候选，`Tab` 补全，`Enter` 执行，`Esc` 取消。列表使用左/右键或 `j/k` 移动，`Enter` 跳转，`Esc` 返回。

每次翻页和跳转都会同步保存当前位置。阅读位置按文本偏移记录；窗口大小改变后会重新分页，并定位到原来阅读的文字。状态保存在当前用户的本地应用数据目录中，解析后的纯文本另存于缓存目录，源文件变化后重建。TXT 自动检测编码并识别常见章节标题；识别不到时整本作为一章。MOBI 不支持 DRM 加密文件。

EPUB 按书内的阅读顺序读取内容；XHTML 的页面标题不会重复计入正文，只有分卷标题而没有正文的页面不会单独占一章。

当前搜索最多显示前 500 条结果。单本书提取后的正文限制为 128 MiB。MOBI 解析已接入，但仍需用你的实际书籍样本确认兼容性。
