# Bolide 从入门到精通

这是 Bolide 语言的系统教程书稿，面向第一次接触 Bolide 的读者，也覆盖 AOT、FFI、并发、内存模型、宏与装饰器、Trait、标准库和工程化实践。

- [完整书稿（Markdown）](./bolide-from-zero-to-mastery.md)
- [PDF 版本](./Bolide从入门到精通.pdf)

建议阅读顺序：

1. 先读第 1–5 章，掌握安装、运行、基础语法和控制流。
2. 再读第 6–12 章，掌握函数、集合、值类型、类、Trait 与运算符重载。
3. 继续读第 13–16 章，学习生成器、宏、装饰器与错误处理。
4. 再读第 17–21 章，理解模块/包、类型系统、内存、并发、FFI 与 AOT。
5. 最后读第 22–27 章，标准库、Web/GUI、调试测试、性能与综合项目。

**版本基准：Bolide 0.14.1**

## 重新导出 PDF

本机需安装 [Pandoc](https://pandoc.org/) 与 [MiKTeX](https://miktex.org/)（提供 `xelatex`），并具备中文字体（如「微软雅黑」）：

```powershell
# 在仓库根目录执行
pandoc docs/book/bolide-from-zero-to-mastery.md `
  -o "docs/book/Bolide从入门到精通.pdf" `
  --pdf-engine=xelatex `
  -V CJKmainfont="Microsoft YaHei" `
  -V mainfont="Microsoft YaHei" `
  -V monofont="Consolas" `
  -V geometry:margin=2.2cm `
  -V fontsize=11pt `
  --toc --toc-depth=2 `
  -V colorlinks=true `
  -V linkcolor=blue `
  --highlight-style=tango `
  -V documentclass=article `
  --metadata title="Bolide 从入门到精通" `
  --metadata author="Bolide Team"
```

或运行：

```powershell
python docs/book/export_pdf.py
```
