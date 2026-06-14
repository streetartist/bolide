package main

import (
	"bytes"
	"flag"
	"html/template"
	"net/http"
	"os"
	"strconv"
	"strings"
	"time"
)

type user struct {
	DisplayName string
	Role        string
	LoggedIn    bool
	IsAdmin     bool
}

type post struct {
	ID        int
	Title     string
	Excerpt   string
	Content   string
	Author    string
	Date      string
	Tags      string
	Cover     string
	Published bool
}

type comment struct {
	PostID  int
	Author  string
	Content string
	Date    string
	Status  string
}

type app struct {
	style    template.HTML
	layout   *template.Template
	index    *template.Template
	about    *template.Template
	post     *template.Template
	login    *template.Template
	posts    []post
	comments []comment
}

var posts = []post{
	{
		ID:        1,
		Title:     "给 Bolide 写一个够用的标准库",
		Excerpt:   "一个小语言的标准库不应该显得笨重，但也要能支撑真实网站、工具和自动化任务。",
		Content:   "Bolide 的标准库目标很明确：普通用户不需要接触裸指针，也不需要理解运行时内部的所有权细节。\n\nWeb、模板和本地数据库这几块能力放在一起后，语言就能写出完整应用。路由、会话、HTML 渲染、持久化行数据都保持在 Bolide 的值语义里，AOT 编译后也能独立运行。",
		Author:    "Bolide 团队",
		Date:      "2026-06-15",
		Tags:      "标准库",
		Cover:     "https://images.unsplash.com/photo-1518005020951-eccb494ad742?auto=format&fit=crop&w=1200&q=80",
		Published: true,
	},
	{
		ID:        2,
		Title:     "模板引擎要保持克制",
		Excerpt:   "默认转义、显式原样输出、循环、条件和点路径，已经能覆盖大多数内容站页面。",
		Content:   "模板引擎最容易失控的地方，是把自己做成另一门复杂语言。这个示例故意只保留几个稳定能力：变量默认 HTML 转义，确实需要时才使用原样输出，数据通过 dict 和 list 传入。\n\n这样模板足够直观，也方便 AOT 与 JIT 共用同一套行为。",
		Author:    "Bolide 团队",
		Date:      "2026-06-14",
		Tags:      "模板",
		Cover:     "https://images.unsplash.com/photo-1497215728101-856f4ea42174?auto=format&fit=crop&w=1200&q=80",
		Published: true,
	},
	{
		ID:        3,
		Title:     "文件数据库先解决早期应用",
		Excerpt:   "CRUD、动态值和稳定 API 先跑起来，未来仍然可以替换成真正的 SQL 后端。",
		Content:   "早期标准库的数据库不必马上和 SQLite 拼性能。它更重要的任务，是让示例和小应用可以可靠地保存数据。\n\n文章、用户、评论和站点设置都能用 typed dynamic 表示。等语言和 ABI 更稳定后，底层再替换为 SQL 后端也不会破坏用户代码。",
		Author:    "Bolide 团队",
		Date:      "2026-06-13",
		Tags:      "数据库",
		Cover:     "https://images.unsplash.com/photo-1516321318423-f06f85e504b3?auto=format&fit=crop&w=1200&q=80",
		Published: true,
	},
	{
		ID:        4,
		Title:     "草稿：后台工作流",
		Excerpt:   "草稿只出现在管理后台，不会进入公开首页。",
		Content:   "这篇草稿用于检查后台列表、编辑表单和发布状态。管理员可以继续编辑它，也可以把它发布到首页。\n\n普通用户不会在公开页面看到草稿内容。",
		Author:    "Bolide 团队",
		Date:      "2026-06-12",
		Tags:      "后台",
		Cover:     "https://images.unsplash.com/photo-1500530855697-b586d89ba3ee?auto=format&fit=crop&w=1200&q=80",
		Published: false,
	},
}

var comments = []comment{
	{PostID: 1, Author: "普通读者", Content: "这个示例已经能覆盖登录、评论和后台审核流程。", Date: "2026-06-15", Status: "已通过"},
	{PostID: 2, Author: "站点管理员", Content: "管理员评论会直接通过，普通用户评论进入后台审核。", Date: "2026-06-15", Status: "已通过"},
	{PostID: 3, Author: "普通读者", Content: "这条评论用于检查待审核列表。", Date: "2026-06-15", Status: "待审核"},
}

func guest() user {
	return user{DisplayName: "游客", Role: "游客"}
}

func loadStyleTag() template.HTML {
	data, err := os.ReadFile("examples/blog/templates/layout.html")
	if err != nil {
		return template.HTML(`<style>body{margin:0;font-family:ui-sans-serif,system-ui,"Microsoft YaHei",sans-serif}.shell{width:min(1120px,calc(100% - 32px));margin:0 auto;padding:36px 0}.grid{display:grid;grid-template-columns:repeat(3,minmax(0,1fr));gap:18px}.post-card,.panel{border:1px solid #d9dee3;border-radius:8px;background:#fff;padding:18px}.hero{min-height:360px;background:#202124;color:#fff;display:flex;align-items:flex-end}.hero-inner{width:min(1120px,calc(100% - 32px));margin:0 auto;padding:72px 0 58px}.btn{display:inline-flex;border:1px solid #202124;border-radius:8px;padding:8px 14px}</style>`)
	}
	text := string(data)
	start := strings.Index(text, "<style>")
	end := strings.Index(text, "</style>")
	if start < 0 || end < start {
		return template.HTML("")
	}
	return template.HTML(text[start : end+len("</style>")])
}

func mustTemplate(name, source string) *template.Template {
	return template.Must(template.New(name).Parse(source))
}

func newApp() *app {
	return &app{
		style:    loadStyleTag(),
		layout:   mustTemplate("layout", layoutTemplate),
		index:    mustTemplate("index", indexTemplate),
		about:    mustTemplate("about", aboutTemplate),
		post:     mustTemplate("post", postTemplate),
		login:    mustTemplate("login", loginTemplate),
		posts:    posts,
		comments: comments,
	}
}

func (a *app) publishedPosts() []post {
	out := make([]post, 0, len(a.posts))
	for _, p := range a.posts {
		if p.Published {
			out = append(out, p)
		}
	}
	return out
}

func (a *app) postByID(id int) (post, bool) {
	for _, p := range a.posts {
		if p.ID == id {
			return p, true
		}
	}
	return post{}, false
}

func (a *app) approvedComments(postID int) []comment {
	out := make([]comment, 0, len(a.comments))
	for _, c := range a.comments {
		if c.PostID == postID && c.Status == "已通过" {
			out = append(out, c)
		}
	}
	return out
}

func (a *app) render(w http.ResponseWriter, title string, page *template.Template, data any) {
	var body bytes.Buffer
	if err := page.Execute(&body, data); err != nil {
		http.Error(w, "template error", http.StatusInternalServerError)
		return
	}

	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	var out bytes.Buffer
	err := a.layout.Execute(&out, map[string]any{
		"Title": title,
		"Style": a.style,
		"Body":  template.HTML(body.String()),
		"User":  guest(),
	})
	if err != nil {
		http.Error(w, "template error", http.StatusInternalServerError)
		return
	}
	_, _ = w.Write(out.Bytes())
}

func (a *app) handleIndex(w http.ResponseWriter, r *http.Request) {
	ps := a.publishedPosts()
	a.render(w, "Bolide 中文博客", a.index, map[string]any{
		"Posts": ps,
		"Count": len(ps),
		"User":  guest(),
	})
}

func (a *app) handleAbout(w http.ResponseWriter, r *http.Request) {
	a.render(w, "关于这个示例", a.about, map[string]any{"User": guest()})
}

func (a *app) handleLogin(w http.ResponseWriter, r *http.Request) {
	a.render(w, "登录", a.login, map[string]any{"Message": "", "User": guest()})
}

func (a *app) handleAdmin(w http.ResponseWriter, r *http.Request) {
	http.Redirect(w, r, "/login", http.StatusSeeOther)
}

func (a *app) handlePost(w http.ResponseWriter, r *http.Request) {
	id, err := strconv.Atoi(r.PathValue("id"))
	if err != nil {
		http.NotFound(w, r)
		return
	}
	p, ok := a.postByID(id)
	if !ok || !p.Published {
		http.NotFound(w, r)
		return
	}
	cs := a.approvedComments(id)
	a.render(w, p.Title, a.post, map[string]any{
		"Post":         p,
		"Comments":     cs,
		"CommentCount": len(cs),
		"User":         guest(),
	})
}

func main() {
	addr := flag.String("addr", "127.0.0.1:18083", "listen address")
	flag.Parse()

	a := newApp()
	mux := http.NewServeMux()
	mux.HandleFunc("GET /", a.handleIndex)
	mux.HandleFunc("GET /about", a.handleAbout)
	mux.HandleFunc("GET /login", a.handleLogin)
	mux.HandleFunc("GET /admin", a.handleAdmin)
	mux.HandleFunc("GET /posts/{id}", a.handlePost)

	server := &http.Server{
		Addr:              *addr,
		Handler:           mux,
		ReadHeaderTimeout: 5 * time.Second,
	}
	if err := server.ListenAndServe(); err != nil && err != http.ErrServerClosed {
		panic(err)
	}
}

const layoutTemplate = `<!doctype html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>{{ .Title }}</title>
  {{ .Style }}
</head>
<body>
  <header class="topbar">
    <nav class="nav">
      <a class="brand" href="/">
        <span class="brand-mark">博</span>
        <span>Bolide 中文博客</span>
      </a>
      <div class="nav-links">
        <a href="/">首页</a>
        <a href="/about">关于</a>
        {{ if .User.IsAdmin }}<a href="/admin">管理后台</a>{{ end }}
      </div>
      <div class="account">
        {{ if .User.LoggedIn }}
          <span class="account-name">{{ .User.DisplayName }}</span>
          <span>{{ .User.Role }}</span>
        {{ else }}
          <a class="btn secondary" href="/login">登录</a>
        {{ end }}
      </div>
    </nav>
  </header>
  <main class="page">{{ .Body }}</main>
  <footer class="footer">
    <div class="footer-inner">
      <span>Bolide 中文博客</span>
      <span>Go 标准库博客对照</span>
    </div>
  </footer>
</body>
</html>`

const indexTemplate = `<section class="hero">
  <div class="hero-inner">
    <p class="eyebrow">工程札记</p>
    <h1>Bolide 中文博客</h1>
    <p>记录语言设计、运行时、AOT 编译、标准库和小型 Web 应用的实践笔记。</p>
  </div>
</section>

<section class="shell">
  <div class="section-head">
    <div>
      <h2>最新文章</h2>
      <p>当前共有 {{ .Count }} 篇已发布文章</p>
    </div>
    {{ if .User.IsAdmin }}<a class="btn" href="/admin/new">新建文章</a>{{ end }}
  </div>

  {{ if .Count }}
    <div class="grid">
      {{ range .Posts }}
        <article class="post-card">
          <a href="/posts/{{ .ID }}">
            <img src="{{ .Cover }}" alt="{{ .Title }}">
          </a>
          <div class="post-card-body">
            <div class="meta">
              <span>{{ .Date }}</span>
              <span>{{ .Author }}</span>
              <span class="tag">{{ .Tags }}</span>
            </div>
            <h3><a href="/posts/{{ .ID }}">{{ .Title }}</a></h3>
            <p>{{ .Excerpt }}</p>
            <a class="read-link" href="/posts/{{ .ID }}">阅读全文</a>
          </div>
        </article>
      {{ end }}
    </div>
  {{ else }}
    <div class="empty">还没有已发布文章。</div>
  {{ end }}
</section>`

const aboutTemplate = `<section class="shell">
  <div class="section-head">
    <div>
      <h1>关于这个示例</h1>
      <p>一个用 Go 标准库写出的完整中文博客对照。</p>
    </div>
  </div>

  <div class="article">
    <div class="article-body">这个示例用于和 Bolide 标准库博客做同机压测对照。

公开页面负责阅读体验；登录后，普通用户可以发表评论；管理员可以新建文章、编辑文章、管理用户，并审核或删除评论。

这里使用 Go 标准库 net/http 与 html/template，数据放在内存里。</div>

    <aside class="side-panel">
      <p><strong>内置账号</strong></p>
      <p>管理员：admin / admin123<br>普通用户：user / user123</p>
    </aside>
  </div>
</section>`

const postTemplate = `<section class="shell">
  <article class="article">
    <div>
      <img class="article-cover" src="{{ .Post.Cover }}" alt="{{ .Post.Title }}">
      <h1>{{ .Post.Title }}</h1>
      <div class="meta">
        <span>{{ .Post.Date }}</span>
        <span>{{ .Post.Author }}</span>
        <span class="tag">{{ .Post.Tags }}</span>
      </div>
      <div class="article-body">{{ .Post.Content }}</div>

      <section class="comments">
        <div class="section-head">
          <div>
            <h2>评论</h2>
            <p>{{ .CommentCount }} 条已通过评论</p>
          </div>
        </div>

        {{ if .CommentCount }}
          <div class="comment-list">
            {{ range .Comments }}
              <div class="comment-item">
                <div class="meta">
                  <strong>{{ .Author }}</strong>
                  <span>{{ .Date }}</span>
                </div>
                <p>{{ .Content }}</p>
              </div>
            {{ end }}
          </div>
        {{ else }}
          <div class="empty">还没有评论。</div>
        {{ end }}

        {{ if .User.LoggedIn }}
          <div class="comment-box">
            <h3>写评论</h3>
            <p>普通用户提交后进入审核，管理员可以在后台通过或删除。</p>
          </div>
        {{ else }}
          <div class="notice">
            登录后可以发表评论。<a class="read-link" href="/login">去登录</a>
          </div>
        {{ end }}
      </section>
    </div>

    <aside class="side-panel">
      <p><strong>摘要</strong></p>
      <p>{{ .Post.Excerpt }}</p>
      <div class="actions">
        <a class="btn secondary" href="/">所有文章</a>
        {{ if .User.IsAdmin }}<a class="btn secondary" href="/admin/edit/{{ .Post.ID }}">编辑文章</a>{{ end }}
      </div>
    </aside>
  </article>
</section>`

const loginTemplate = `<section class="shell">
  <div class="section-head">
    <div>
      <h1>登录</h1>
      <p>管理员可以进入后台，普通用户可以发表评论。</p>
    </div>
  </div>

  <div class="login-card">
    {{ if .Message }}<div class="notice error">{{ .Message }}</div>{{ end }}
    <form class="login-form" method="post" action="/login">
      <div class="field">
        <label for="username">用户名</label>
        <input id="username" name="username" autocomplete="username">
      </div>
      <div class="field">
        <label for="password">密码</label>
        <input id="password" type="password" name="password" autocomplete="current-password">
      </div>
      <div class="actions">
        <button class="btn" type="submit">登录</button>
      </div>
    </form>
  </div>
</section>`
