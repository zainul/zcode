// Package httpapi exposes the order service over HTTP.
package httpapi

import (
	"encoding/json"
	"errors"
	"net/http"
	"strings"

	"example.com/orders/internal/orders"
)

// Handler routes /orders requests to the service.
type Handler struct {
	svc *orders.Service
}

// New builds a Handler.
func New(svc *orders.Service) *Handler {
	return &Handler{svc: svc}
}

type createRequest struct {
	Items       []orders.Item `json:"items"`
	DiscountPct int           `json:"discount_pct"`
}

type orderResponse struct {
	orders.Order
	TotalCents int64 `json:"total_cents"`
}

func (h *Handler) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	id := strings.TrimPrefix(r.URL.Path, "/orders/")
	switch {
	case r.Method == http.MethodPost && r.URL.Path == "/orders":
		h.create(w, r)
	case r.Method == http.MethodGet && id != "" && id != r.URL.Path:
		h.get(w, id)
	default:
		http.NotFound(w, r)
	}
}

func (h *Handler) create(w http.ResponseWriter, r *http.Request) {
	var req createRequest
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		http.Error(w, "invalid JSON: "+err.Error(), http.StatusBadRequest)
		return
	}
	o, err := h.svc.Create(req.Items, req.DiscountPct)
	if err != nil {
		http.Error(w, err.Error(), http.StatusUnprocessableEntity)
		return
	}
	writeJSON(w, http.StatusCreated, orderResponse{Order: o, TotalCents: o.Total()})
}

func (h *Handler) get(w http.ResponseWriter, id string) {
	o, err := h.svc.Get(id)
	if errors.Is(err, orders.ErrNotFound) {
		http.Error(w, "not found", http.StatusNotFound)
		return
	}
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	writeJSON(w, http.StatusOK, orderResponse{Order: o, TotalCents: o.Total()})
}

func writeJSON(w http.ResponseWriter, status int, v any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(v)
}
