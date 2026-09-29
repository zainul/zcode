package orders

import (
	"errors"
	"sync"
)

// ErrNotFound is returned when no order has the requested id.
var ErrNotFound = errors.New("order not found")

// Store persists orders.
type Store interface {
	Get(id string) (Order, error)
	Put(o Order) error
}

// MemoryStore is an in-memory Store, safe for concurrent use.
type MemoryStore struct {
	mu     sync.RWMutex
	orders map[string]Order
}

// NewMemoryStore returns an empty store.
func NewMemoryStore() *MemoryStore {
	return &MemoryStore{orders: make(map[string]Order)}
}

// Get returns the order with the given id.
func (s *MemoryStore) Get(id string) (Order, error) {
	s.mu.RLock()
	defer s.mu.RUnlock()
	o, ok := s.orders[id]
	if !ok {
		return Order{}, ErrNotFound
	}
	return o, nil
}

// Put inserts or replaces an order.
func (s *MemoryStore) Put(o Order) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.orders[o.ID] = o
	return nil
}
