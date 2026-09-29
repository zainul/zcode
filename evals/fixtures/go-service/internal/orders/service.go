package orders

import (
	"errors"
	"fmt"
)

// ErrEmptyOrder is returned when an order has no items.
var ErrEmptyOrder = errors.New("order has no items")

// Service is the application logic around orders.
type Service struct {
	store Store
	next  int
}

// NewService builds a Service over a Store.
func NewService(store Store) *Service {
	return &Service{store: store}
}

// Create validates and stores a new open order, assigning its id.
func (s *Service) Create(items []Item, discountPct int) (Order, error) {
	if len(items) == 0 {
		return Order{}, ErrEmptyOrder
	}
	if discountPct < 0 || discountPct > 100 {
		return Order{}, fmt.Errorf("discount %d%% out of range", discountPct)
	}
	s.next++
	o := Order{
		ID:          fmt.Sprintf("ord-%d", s.next),
		Items:       items,
		DiscountPct: discountPct,
		Status:      StatusOpen,
	}
	if err := s.store.Put(o); err != nil {
		return Order{}, err
	}
	return o, nil
}

// Get returns a stored order.
func (s *Service) Get(id string) (Order, error) {
	return s.store.Get(id)
}
