"""
Connection pooling for Qdrant client to improve performance and resource management.
"""

import asyncio
from typing import Optional, Any
from contextlib import asynccontextmanager
import logging
from qdrant_client import AsyncQdrantClient

logger = logging.getLogger(__name__)


class QdrantConnectionPool:
    """
    A connection pool for Qdrant clients with configurable size and timeout.
    """
    
    def __init__(
        self, 
        url: str, 
        pool_size: int = 10,
        max_overflow: int = 5,
        timeout: float = 30.0,
        retry_attempts: int = 3,
        retry_delay: float = 1.0
    ):
        """
        Initialize the connection pool.
        
        Args:
            url: Qdrant server URL
            pool_size: Base number of connections to maintain
            max_overflow: Additional connections that can be created if pool is exhausted
            timeout: Timeout for acquiring a connection from the pool
            retry_attempts: Number of retry attempts for failed operations
            retry_delay: Delay between retry attempts (with exponential backoff)
        """
        self.url = url
        self.pool_size = pool_size
        self.max_overflow = max_overflow
        self.timeout = timeout
        self.retry_attempts = retry_attempts
        self.retry_delay = retry_delay
        
        # Connection pool
        self._pool = asyncio.Queue(maxsize=pool_size)
        self._overflow_connections = []
        self._semaphore = asyncio.Semaphore(pool_size + max_overflow)
        self._initialized = False
        self._lock = asyncio.Lock()
        
        # Statistics
        self.stats = {
            'connections_created': 0,
            'connections_reused': 0,
            'connections_failed': 0,
            'overflow_used': 0,
            'timeouts': 0
        }
    
    async def initialize(self):
        """Initialize the connection pool with base connections."""
        async with self._lock:
            if self._initialized:
                return
            
            # Create initial pool connections
            for _ in range(self.pool_size):
                try:
                    client = AsyncQdrantClient(url=self.url)
                    await self._pool.put(client)
                    self.stats['connections_created'] += 1
                except Exception as e:
                    logger.error(f"Failed to create initial connection: {e}")
                    self.stats['connections_failed'] += 1
            
            self._initialized = True
            logger.info(f"Connection pool initialized with {self._pool.qsize()} connections")
    
    @asynccontextmanager
    async def acquire(self):
        """
        Acquire a connection from the pool.
        
        Yields:
            AsyncQdrantClient instance
        """
        if not self._initialized:
            await self.initialize()
        
        client = None
        acquired_from_overflow = False
        
        try:
            # Try to get a connection with timeout
            try:
                client = await asyncio.wait_for(
                    self._pool.get(),
                    timeout=self.timeout
                )
                self.stats['connections_reused'] += 1
            except asyncio.TimeoutError:
                # Pool is exhausted, try overflow
                self.stats['timeouts'] += 1
                
                if len(self._overflow_connections) < self.max_overflow:
                    # Create overflow connection
                    logger.debug("Creating overflow connection")
                    client = AsyncQdrantClient(url=self.url)
                    self._overflow_connections.append(client)
                    acquired_from_overflow = True
                    self.stats['overflow_used'] += 1
                    self.stats['connections_created'] += 1
                else:
                    raise RuntimeError("Connection pool exhausted and max overflow reached")
            
            # Yield the client for use
            yield client
            
        finally:
            # Return connection to pool
            if client is not None:
                if acquired_from_overflow:
                    # Remove from overflow list
                    if client in self._overflow_connections:
                        self._overflow_connections.remove(client)
                else:
                    # Return to pool
                    try:
                        await self._pool.put(client)
                    except asyncio.QueueFull:
                        # This shouldn't happen, but handle gracefully
                        logger.warning("Connection pool is full, closing extra connection")
                        # In production, we might want to close the client here
    
    async def execute_with_retry(self, func, *args, **kwargs):
        """
        Execute a function with retry logic and exponential backoff.
        
        Args:
            func: Async function to execute
            *args: Positional arguments for the function
            **kwargs: Keyword arguments for the function
        
        Returns:
            Result from the function
        """
        last_exception = None
        delay = self.retry_delay
        
        for attempt in range(self.retry_attempts):
            try:
                async with self.acquire() as client:
                    # Pass the client as the first argument
                    return await func(client, *args, **kwargs)
            except Exception as e:
                last_exception = e
                if attempt < self.retry_attempts - 1:
                    logger.warning(f"Attempt {attempt + 1} failed: {e}. Retrying in {delay}s...")
                    await asyncio.sleep(delay)
                    delay *= 2  # Exponential backoff
                else:
                    logger.error(f"All {self.retry_attempts} attempts failed: {e}")
        
        raise last_exception
    
    async def close(self):
        """Close all connections in the pool."""
        async with self._lock:
            # Close all pooled connections
            while not self._pool.empty():
                try:
                    client = await self._pool.get()
                    # AsyncQdrantClient doesn't have a close method, but we can del it
                    del client
                except Exception as e:
                    logger.error(f"Error closing connection: {e}")
            
            # Close overflow connections
            for client in self._overflow_connections:
                try:
                    del client
                except Exception as e:
                    logger.error(f"Error closing overflow connection: {e}")
            
            self._overflow_connections.clear()
            self._initialized = False
            logger.info("Connection pool closed")
    
    def get_stats(self) -> dict:
        """Get pool statistics."""
        return {
            **self.stats,
            'current_pool_size': self._pool.qsize() if self._initialized else 0,
            'overflow_active': len(self._overflow_connections),
            'initialized': self._initialized
        }


# Circuit breaker implementation for additional resilience
class CircuitBreaker:
    """
    Circuit breaker pattern to prevent cascading failures.
    """
    
    def __init__(
        self,
        failure_threshold: int = 5,
        recovery_timeout: float = 60.0,
        expected_exception: type = Exception
    ):
        """
        Initialize circuit breaker.
        
        Args:
            failure_threshold: Number of failures before opening circuit
            recovery_timeout: Time to wait before attempting recovery
            expected_exception: Exception type to catch
        """
        self.failure_threshold = failure_threshold
        self.recovery_timeout = recovery_timeout
        self.expected_exception = expected_exception
        
        self._failure_count = 0
        self._last_failure_time = None
        self._state = 'closed'  # closed, open, half_open
        self._lock = asyncio.Lock()
    
    async def call(self, func, *args, **kwargs):
        """
        Call a function through the circuit breaker.
        
        Args:
            func: Async function to call
            *args: Positional arguments
            **kwargs: Keyword arguments
        
        Returns:
            Result from function
        
        Raises:
            CircuitBreakerOpen: If circuit is open
        """
        async with self._lock:
            # Check circuit state
            if self._state == 'open':
                # Check if we should try half-open
                if self._last_failure_time:
                    time_since_failure = asyncio.get_event_loop().time() - self._last_failure_time
                    if time_since_failure > self.recovery_timeout:
                        self._state = 'half_open'
                        logger.info("Circuit breaker entering half-open state")
                    else:
                        raise CircuitBreakerOpen(f"Circuit breaker is open (failures: {self._failure_count})")
        
        try:
            # Attempt the call
            result = await func(*args, **kwargs)
            
            # Success - update state
            async with self._lock:
                if self._state == 'half_open':
                    self._state = 'closed'
                    logger.info("Circuit breaker closed after successful recovery")
                self._failure_count = 0
                self._last_failure_time = None
            
            return result
            
        except self.expected_exception as e:
            # Failure - update state
            async with self._lock:
                self._failure_count += 1
                self._last_failure_time = asyncio.get_event_loop().time()
                
                if self._failure_count >= self.failure_threshold:
                    self._state = 'open'
                    logger.error(f"Circuit breaker opened after {self._failure_count} failures")
                
                raise e


class CircuitBreakerOpen(Exception):
    """Exception raised when circuit breaker is open."""
    pass