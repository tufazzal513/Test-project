package admin

import (
	"net/http/httptest"
	"testing"
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
