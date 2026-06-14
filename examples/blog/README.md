# Bolide 中文博客示例

请从本目录运行，这样应用可以读取 `templates/` 并把数据写入 `data/`：

```bash
bolide run main.bl
```

AOT 编译：

```bash
bolide compile main.bl -o blog_site.exe
```

内置账号：

- 管理员：admin / admin123
- 普通用户：user / user123

管理员可以新建文章、编辑文章、管理用户、审核和删除评论。普通用户登录后可以发表评论。
