package admin

import (
	"crypto/hmac"
	"crypto/sha256"
	"encoding/base64"
	"net/http"
	"net/url"
	"strconv"
	"strings"
	"time"

	"movex/internal/httpx"
)

// Session is the signed-in admin of the current request.
type Session struct {
	ID   string // the secret session id (never logged)
	UID  string
	CSRF string
}

func (a *Admin) mac(purpose, data string) string {
	m := hmac.New(sha256.New, a.deps.Cfg.SessionSecret)
	m.Write([]byte(purpose))
	m.Write([]byte{0})
	m.Write([]byte(data))
	return base64.RawURLEncoding.EncodeToString(m.Sum(nil))
}

func (a *Admin) macOK(purpose, data, sig string) bool {
	return hmac.Equal([]byte(a.mac(purpose, data)), []byte(sig))
}

func (a *Admin) csrfFor(sessionID string) string { return a.mac("csrf", sessionID) }

// loginToken is the stateless CSRF token of the (not yet signed-in) login form: "<unix>.<mac>", valid 2 hours.
func (a *Admin) loginToken() string {
	ts := strconv.FormatInt(a.now().Unix(), 10)
	return ts + "." + a.mac("login", ts)
}

func (a *Admin) loginTokenOK(tok string) bool {
	ts, sig, ok := strings.Cut(tok, ".")
	if !ok || !a.macOK("login", ts, sig) {
		return false
	}
	n, err := strconv.ParseInt(ts, 10, 64)
	age := a.now().Sub(time.Unix(n, 0))
	return err == nil && age >= -time.Minute && age < 2*time.Hour
}

func (a *Admin) setSessionCookie(w http.ResponseWriter, id string) {
	http.SetCookie(w, &http.Cookie{
		Name: sessionCookie, Value: id + "." + a.mac("session", id), Path: "/admin",
		MaxAge: int(sessionTTL.Seconds()), HttpOnly: true, Secure: a.deps.Cfg.CookieSecure, SameSite: http.SameSiteStrictMode,
	})
}

func (a *Admin) clearSessionCookie(w http.ResponseWriter) {
	http.SetCookie(w, &http.Cookie{
		Name: sessionCookie, Value: "", Path: "/admin", MaxAge: -1, HttpOnly: true, Secure: a.deps.Cfg.CookieSecure, SameSite: http.SameSiteStrictMode,
	})
}

// sessionFrom returns the signed-in admin of the request, or false. A cookie with a bad signature never reaches the
// database; a valid one is looked up, and the uid must still be listed in ADMIN_UIDS.
func (a *Admin) sessionFrom(r *http.Request) (*Session, bool) {
	ck, err := r.Cookie(sessionCookie)
	if err != nil || len(ck.Value) > 200 {
		return nil, false
	}
	id, sig, ok := strings.Cut(ck.Value, ".")
	if !ok || !a.macOK("session", id, sig) {
		return nil, false
	}
	s, err := a.deps.Repo.GetAdminSession(r.Context(), id)
	if err != nil || !a.admins[s.UID] {
		return nil, false
	}
	return &Session{ID: id, UID: s.UID, CSRF: a.csrfFor(id)}, true
}

// safeNext keeps only a local path inside /admin (plus query), so a login link can never send the browser to
// another site or to a sibling path such as /administrator.
func safeNext(next string) string {
	const fallback = "/admin/"
	if next == "" || len(next) > 500 || strings.ContainsAny(next, "\\\r\n\t ") {
		return fallback
	}
	u, err := url.Parse(next)
	if err != nil || u.Scheme != "" || u.Host != "" || u.User != nil {
		return fallback
	}
	p := u.Path
	if p != "/admin" && !strings.HasPrefix(p, "/admin/") {
		return fallback
	}
	if strings.Contains(p, "//") || strings.Contains(p, "/../") || strings.HasSuffix(p, "/..") ||
		p == "/admin/login" || p == "/admin/logout" {
		return fallback
	}
	return next
}

// sameOrigin is the first CSRF layer: browsers tell us where a request came from.
// When running behind an explicitly trusted reverse proxy, compare Origin with
// the public host forwarded by that proxy instead of the private upstream Host.
func sameOrigin(r *http.Request, trustProxy bool) bool {
	// Fetch Metadata is an additional browser signal. If present, only an
	// explicitly same-origin request may reach an unsafe admin handler.
	if sfs := strings.TrimSpace(r.Header.Get("Sec-Fetch-Site")); sfs != "" && sfs != "same-origin" {
		return false
	}

	expectedHost := r.Host
	if trustProxy {
		if forwardedHost := strings.TrimSpace(r.Header.Get("X-Forwarded-Host")); forwardedHost != "" {
			// Accept exactly one authority from the trusted proxy. Never use
			// a comma-separated client-supplied chain for security decisions.
			if strings.ContainsAny(forwardedHost, ",\r\n\t /\\") {
				return false
			}
			fh, err := url.Parse("//" + forwardedHost)
			if err != nil || fh.Host == "" || fh.User != nil || fh.Path != "" || fh.RawQuery != "" || fh.Fragment != "" {
				return false
			}
			expectedHost = fh.Host
		}
	}

	origin := strings.TrimSpace(r.Header.Get("Origin"))
	if origin == "null" {
		// Some embedded/sandboxed browser contexts serialize Origin as "null".
		// Do not trust that value by itself: require Fetch Metadata and a valid
		// same-host Referer. The login/session CSRF token remains mandatory too.
		if r.Header.Get("Sec-Fetch-Site") != "same-origin" {
			return false
		}
		origin = strings.TrimSpace(r.Header.Get("Referer"))
		if origin == "" {
			return false
		}
	} else if origin == "" {
		// Older non-browser clients may omit Origin. Admin unsafe requests still
		// require their session/login CSRF token at the handler layer.
		return true
	}

	u, err := url.Parse(origin)
	if err != nil || (u.Scheme != "http" && u.Scheme != "https") || u.Host == "" || u.User != nil {
		return false
	}
	return strings.EqualFold(u.Host, expectedHost)
}

func safeMethod(m string) bool {
	return m == http.MethodGet || m == http.MethodHead || m == http.MethodOptions
}

// requireAdmin builds the request context for a signed-in admin and applies the CSRF checks to unsafe methods.
func (a *Admin) requireAdmin(fn func(*Ctx)) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		sess, ok := a.sessionFrom(r)
		if !ok {
			a.clearIfPresent(w, r)
			a.denyAnonymous(w, r)
			return
		}
		httpx.SetUID(r, sess.UID)
		if !safeMethod(r.Method) {
			limit := int64(maxFormBytes)
			if r.URL.Path == "/admin/server/restore" {
				limit = 512 << 20
			}
			r.Body = http.MaxBytesReader(w, r.Body, limit)
			if !sameOrigin(r, a.deps.Cfg.TrustProxy) || !a.csrfOK(r, sess) {
				a.deps.Log.Warn("admin: CSRF check failed", "id", httpx.RequestID(r), "method", r.Method, "path", r.URL.Path, "uid", sess.UID)
				c := &Ctx{W: w, R: r, Admin: a, Session: sess}
				c.Error(http.StatusForbidden, "Security check failed. Reload the page and try again.")
				return
			}
		}
		fn(&Ctx{W: w, R: r, Admin: a, Session: sess})
	})
}

func (a *Admin) csrfOK(r *http.Request, s *Session) bool {
	tok := r.Header.Get("X-CSRF-Token")
	if tok == "" {
		if ct := r.Header.Get("Content-Type"); strings.HasPrefix(ct, "multipart/form-data") {
			if err := r.ParseMultipartForm(1 << 20); err == nil {
				tok = r.PostForm.Get("csrf")
			}
		} else if strings.HasPrefix(ct, "application/x-www-form-urlencoded") {
			if err := r.ParseForm(); err == nil {
				tok = r.PostForm.Get("csrf")
			}
		}
	}
	return tok != "" && hmac.Equal([]byte(tok), []byte(s.CSRF))
}

func (a *Admin) clearIfPresent(w http.ResponseWriter, r *http.Request) {
	if _, err := r.Cookie(sessionCookie); err == nil {
		a.clearSessionCookie(w)
	}
}

// denyAnonymous sends visitors without a valid session to the login page (htmx gets a full-page redirect).
func (a *Admin) denyAnonymous(w http.ResponseWriter, r *http.Request) {
	if r.Header.Get("HX-Request") == "true" {
		w.Header().Set("HX-Redirect", "/admin/login")
		w.WriteHeader(http.StatusUnauthorized)
		return
	}
	target := "/admin/login"
	if r.Method == http.MethodGet {
		if next := safeNext(r.URL.RequestURI()); next != "/admin/" {
			target += "?next=" + url.QueryEscape(next)
		}
	}
	http.Redirect(w, r, target, http.StatusSeeOther)
}

// secure adds the admin response headers: no caching, a strict CSP, no framing, no sniffing.
func (a *Admin) secure(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		h := w.Header()
		h.Set("Cache-Control", "no-store")
		h.Set("Content-Security-Policy", csp)
		h.Set("X-Frame-Options", "DENY")
		h.Set("X-Content-Type-Options", "nosniff")
		h.Set("Referrer-Policy", "same-origin")
		h.Set("Cross-Origin-Opener-Policy", "same-origin")
		h.Set("Permissions-Policy", "camera=(), microphone=(), geolocation=(), payment=()")
		if a.deps.Cfg.CookieSecure {
			h.Set("Strict-Transport-Security", "max-age=15552000")
		}
		next.ServeHTTP(w, r)
	})
}

// No inline script, no inline style, no third-party host. Images may come from any https site because the
// movie form previews posters that are hosted elsewhere.
const csp = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data: https:; font-src 'self'; " +
	"connect-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'; object-src 'none'"
