package httpapi

import (
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"example.com/orders/internal/orders"
)

func TestCreateReturns201(t *testing.T) {
	h := New(orders.NewService(orders.NewMemoryStore()))
	body := `{"items":[{"sku":"a","quantity":1,"unit_cents":100}]}`
	rec := httptest.NewRecorder()
	h.ServeHTTP(rec, httptest.NewRequest(http.MethodPost, "/orders", strings.NewReader(body)))
	if rec.Code != http.StatusCreated {
		t.Fatalf("status = %d, body %s", rec.Code, rec.Body)
	}
}

func TestCreateRejectsBadJSON(t *testing.T) {
	h := New(orders.NewService(orders.NewMemoryStore()))
	rec := httptest.NewRecorder()
	h.ServeHTTP(rec, httptest.NewRequest(http.MethodPost, "/orders", strings.NewReader("{")))
	if rec.Code != http.StatusBadRequest {
		t.Fatalf("status = %d", rec.Code)
	}
}
