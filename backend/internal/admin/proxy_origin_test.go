package admin

import (
	"net/http"
	"net/http/httptest"
	"testing"

	"movex/internal/config"
)

func TestSameOriginBehindTrustedProxy(t *testing.T) {
	r := httptest.NewRequest("POST", "http://127.0.0.1:43969/admin/login", nil)
	r.Header.Set("Origin", "https://d3g3pb.bot-keep.xyz")
	r.Header.Set("Sec-Fetch-Site", "same-origin")
	r.Header.Set("X-Forwarded-Host", "d3g3pb.bot-keep.xyz")

	if sameOrigin(r, false) {
		t.Fatal("must not trust forwarded host when TRUST_PROXY is disabled")
	}
	if !sameOrigin(r, true) {
		t.Fatal("trusted proxy public host should match Origin")
	}
}

func TestSameOriginRejectsUntrustedOrAmbiguousForwardedHost(t *testing.T) {
	for _, host := range []string{
		"attacker.example",
		"d3g3pb.bot-keep.xyz, attacker.example",
		"d3g3pb.bot-keep.xyz/path",
		"",
	} {
		r := httptest.NewRequest("POST", "http://127.0.0.1:43969/admin/login", nil)
		r.Header.Set("Origin", "https://d3g3pb.bot-keep.xyz")
		r.Header.Set("X-Forwarded-Host", host)
		got := sameOrigin(r, true)
		want := host == "d3g3pb.bot-keep.xyz"
		if got != want {
			t.Errorf("X-Forwarded-Host %q: got %v, want %v", host, got, want)
		}
	}
}

func TestSameOriginRejectsCrossSiteFetch(t *testing.T) {
	r := httptest.NewRequest("POST", "https://d3g3pb.bot-keep.xyz/admin/login", nil)
	r.Header.Set("Origin", "https://d3g3pb.bot-keep.xyz")
	r.Header.Set("Sec-Fetch-Site", "cross-site")
	if sameOrigin(r, true) {
		t.Fatal("cross-site fetch must be rejected")
	}
}

func TestSameOriginNullOriginRequiresSameOriginReferer(t *testing.T) {
	tests := []struct {
		name      string
		referer   string
		fetchSite string
		want      bool
	}{
		{name: "same host referer and same-origin metadata", referer: "https://d3g3pb.bot-keep.xyz/admin/login", fetchSite: "same-origin", want: true},
		{name: "foreign referer", referer: "https://attacker.example/admin/login", fetchSite: "same-origin", want: false},
		{name: "missing referer", fetchSite: "same-origin", want: false},
		{name: "cross-site metadata", referer: "https://d3g3pb.bot-keep.xyz/admin/login", fetchSite: "cross-site", want: false},
		{name: "same-site is not same-origin", referer: "https://d3g3pb.bot-keep.xyz/admin/login", fetchSite: "same-site", want: false},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			r := httptest.NewRequest("POST", "http://127.0.0.1:43969/admin/login", nil)
			r.Host = "127.0.0.1:43969"
			r.Header.Set("Origin", "null")
			r.Header.Set("Sec-Fetch-Site", tt.fetchSite)
			r.Header.Set("Referer", tt.referer)
			r.Header.Set("X-Forwarded-Host", "d3g3pb.bot-keep.xyz")
			if got := sameOrigin(r, true); got != tt.want {
				t.Fatalf("sameOrigin() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestSameOriginNullOriginRejectsMalformedReferer(t *testing.T) {
	for _, referer := range []string{"null", "javascript:alert(1)", "https://user@d3g3pb.bot-keep.xyz/", "https://d3g3pb.bot-keep.xyz.evil.example/"} {
		r := httptest.NewRequest("POST", "http://127.0.0.1:43969/admin/login", nil)
		r.Header.Set("Origin", "null")
		r.Header.Set("Sec-Fetch-Site", "same-origin")
		r.Header.Set("Referer", referer)
		r.Header.Set("X-Forwarded-Host", "d3g3pb.bot-keep.xyz")
		if sameOrigin(r, true) {
			t.Errorf("sameOrigin() accepted malformed/foreign Referer %q", referer)
		}
	}
}

func TestRefererOriginForLogDoesNotExposePathOrQuery(t *testing.T) {
	got := refererOriginForLog("https://d3g3pb.bot-keep.xyz/admin/login?token=secret")
	if got != "https://d3g3pb.bot-keep.xyz" {
		t.Fatalf("refererOriginForLog() = %q", got)
	}
	if got := refererOriginForLog(""); got != "missing" {
		t.Fatalf("missing Referer label = %q", got)
	}
	if got := refererOriginForLog("not a url"); got != "invalid" {
		t.Fatalf("invalid Referer label = %q", got)
	}
}

func TestSecurePolicyAllowsSameOriginRefererOnly(t *testing.T) {
	a := &Admin{deps: Deps{Cfg: &config.Config{CookieSecure: true}}}
	h := a.secure(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) { w.WriteHeader(http.StatusNoContent) }))
	rr := httptest.NewRecorder()
	h.ServeHTTP(rr, httptest.NewRequest(http.MethodGet, "https://d3g3pb.bot-keep.xyz/admin/login", nil))
	if got := rr.Header().Get("Referrer-Policy"); got != "same-origin" {
		t.Fatalf("Referrer-Policy = %q, want same-origin", got)
	}
}
